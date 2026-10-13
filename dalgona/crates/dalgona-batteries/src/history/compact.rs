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
pub(crate) use super::pipeline::{
    BlobStore, Budget, Commit, Decline, Drawn, DrawnLetter, Engine, KnownLetter, Limits, Request,
    Slot, SourceReader,
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

    /// Returns the lazy source for earlier PNG blobs.
    fn blob_store(
        &self,
        _services: &Arc<dyn Services>,
        _caller: &Caller,
    ) -> Option<Arc<dyn BlobStore>> {
        None
    }

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

    fn blob_store(
        &self,
        services: &Arc<dyn Services>,
        caller: &Caller,
    ) -> Option<Arc<dyn BlobStore>> {
        Some(Arc::new(SessionBlobs {
            services: Arc::clone(services),
            caller: caller.clone(),
        }))
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

    fn get(&self, digest: [u8; 32]) -> BoxFuture<'_, Result<Option<Vec<u8>>, CompactError>> {
        let get = self.services.blob_get(&self.caller, digest);
        Box::pin(async move { get.await.map_err(CompactError::from) })
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
            let known = known_letters(&services, input.caller, next).await?;
            let blobs = self.host.blob_store(&services, input.caller);
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
            if let Some(blobs) = blobs {
                request = request.with_blob_store(blobs);
            }
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
/// Every record is decoded so malformed history still refuses the image path.
/// Only compaction records enter the reuse index; their PNGs are loaded only
/// after the pipeline has identified a matching candidate.
async fn known_letters(
    services: &Arc<dyn Services>,
    caller: &Caller,
    next: EntryId,
) -> Result<Vec<KnownLetter>, CompactError> {
    let bodies = services.records(caller, "letter").await?;
    let mut known = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let record = LetterRecord::decode(body, next).map_err(Decline::from)?;
        if matches!(record, LetterRecord::Compaction { .. }) {
            known.push(KnownLetter { record, png: None });
        }
    }
    Ok(known)
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

#[cfg(test)]
mod boundary_tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dal_agent::error::ServiceError;
    use dal_agent::ext::services::ServiceFuture;
    use dal_agent::ext::{Caller, Doc, EventStream, RawValue, Services, Tool, ToolCx, ToolOutcome};
    use dal_core::ext::McpDeclaration;
    use dal_core::{
        AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
        JobsReply, McpRequest, McpResponse, ModelRequest, Notice, Question, RawJson, RunOutput,
        RunRequest, SidecarOp, StateError, StateOp, StateRecord, TurnOp, TurnOpReply, Visibility,
    };

    use super::known_letters;

    fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
        Box::pin(async { Err(ServiceError::failed(None, "unused in boundary test")) })
    }

    struct BoundaryServices {
        records: Vec<RawJson>,
        blob_gets: AtomicUsize,
    }

    impl Services for BoundaryServices {
        fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }
        fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            unavailable()
        }
        fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
            unavailable()
        }
        fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
            unavailable()
        }
        fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
            unavailable()
        }
        fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
            unavailable()
        }
        fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
            unavailable()
        }
        fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
            unavailable()
        }
        fn add_session_tools(
            &self,
            _who: &Caller,
            _tools: Vec<(Arc<dyn Tool>, Visibility)>,
        ) -> ServiceFuture<'_, ()> {
            unavailable()
        }
        fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
            unavailable()
        }
        fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
            unavailable()
        }
        fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
            unavailable()
        }
        fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
            unavailable()
        }
        fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
            unavailable()
        }
        fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
            unavailable()
        }
        fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }
        fn state(
            &self,
            _who: &Caller,
            _op: StateOp,
        ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
            unavailable()
        }
        fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
            unavailable()
        }
        fn infer_stream(
            &self,
            _who: &Caller,
            _req: ModelRequest,
        ) -> ServiceFuture<'_, EventStream> {
            unavailable()
        }
        fn call_tool(
            &self,
            _who: &Caller,
            _name: &str,
            _args: Box<RawValue>,
        ) -> ServiceFuture<'_, ToolOutcome> {
            unavailable()
        }
        fn notify(&self, _who: &Caller, _notice: Notice) {}
        fn append_record(
            &self,
            _who: &Caller,
            _kind: &str,
            _body: Box<RawValue>,
        ) -> ServiceFuture<'_, EntryId> {
            unavailable()
        }
        fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
            let records = self.records.iter().cloned().map(Box::new).collect();
            Box::pin(async move { Ok(records) })
        }
        fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
            unavailable()
        }
        fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
            self.blob_gets.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }
    }

    #[tokio::test]
    async fn metadata_scan_does_not_fetch_unrelated_pngs() {
        let digest = "a".repeat(64);
        let mut records = Vec::with_capacity(102);
        for ordinal in 1..=100 {
            records.push(
                RawJson::parse(&format!(
                    r#"{{"v":1,"id":"history/{ordinal}.1","kind":"compaction","png_blob":"{digest}","png_bytes":1,"width":136,"height":16,"cell":[8,16],"spans":[[1,0,0,1]],"letters":[1]}}"#
                ))
                .expect("compaction record"),
            );
        }
        records.push(
            RawJson::parse(
                r#"{"v":1,"id":"skill","kind":"skill","png_blob":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","png_bytes":1,"width":136,"height":16,"cell":[8,16],"spans":[]}"#,
            )
            .expect("skill record"),
        );
        records.push(
            RawJson::parse(
                r#"{"v":1,"id":"dream/1","kind":"dream","letters":["history/1.1"],"summary":"old"}"#,
            )
            .expect("dream record"),
        );
        let raw_services = Arc::new(BoundaryServices {
            records,
            blob_gets: AtomicUsize::new(0),
        });
        let services: Arc<dyn Services> = Arc::clone(&raw_services) as Arc<dyn Services>;
        let cx = ToolCx::for_test(Arc::clone(&services));
        let known = known_letters(
            &services,
            cx.caller(),
            EntryId::new(NonZeroU64::new(2).expect("nonzero")),
        )
        .await
        .expect("metadata decodes");
        assert_eq!(known.len(), 100);
        assert!(
            known
                .iter()
                .all(|letter| letter.record.id().starts_with("history/"))
        );
        assert_eq!(raw_services.blob_gets.load(Ordering::SeqCst), 0);
    }
}
