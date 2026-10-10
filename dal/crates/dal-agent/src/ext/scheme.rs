//! Extension scheme runtime: resolver trait, host-minted context, document.
//!
//! One scheme registers as a name plus an [`SchemeResolver`] pair via
//! `ExtensionBuilder::scheme`; at read time the host parses the URI,
//! mints one [`SchemeCx`] for the call, and drives the returned future
//! to completion. Resolvers perform bounded, non-blocking reads through
//! this context only; they never mint a [`Caller`], spawn tasks, open
//! queues, or touch the journal or blob store directly.

use std::marker::PhantomData;
use std::sync::Arc;

use dal_core::{BlobId, SessionId};

use crate::error::SchemeError;

use super::docs::{DocPage, DocTable};
use super::{BoxFuture, Caller, Services};

/// One scheme document: the full URI plus its display text.
///
/// The host's core schemes (`session://`, `job://`) and every extension
/// scheme return this same value; `dal-tools` reads [`Doc::text`]
/// directly for page display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Doc {
    /// The full URI that was resolved, including the `://` separator.
    pub uri: Box<str>,
    /// The document body: source text, Markdown, or JSON per scheme.
    pub text: Box<str>,
}

impl Doc {
    /// Builds a document from its URI and body text.
    #[must_use]
    pub fn new(uri: impl Into<Box<str>>, text: impl Into<Box<str>>) -> Self {
        Self {
            uri: uri.into(),
            text: text.into(),
        }
    }

    /// Builds a document with an empty body (for an empty index).
    #[must_use]
    pub fn empty(uri: impl Into<Box<str>>) -> Self {
        Self {
            uri: uri.into(),
            text: Box::<str>::default(),
        }
    }
}

/// One scheme implementation.
///
/// Registered alongside its scheme name; the host calls [`SchemeResolver::read`]
/// with the URI path after `://` and a host-minted context. Returning
/// [`SchemeError::NotFound`] reports an unknown path; [`SchemeError::Failed`]
/// carries the scheme's ordinary product text.
pub trait SchemeResolver: Send + Sync + 'static {
    /// Reads `path` (the URI after `://`) to a document.
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>>;

    /// Reads `path` without a session, when the scheme's content is
    /// generation-static. `None` means the scheme needs session context;
    /// host-level `doc()` then falls back to the static page table.
    fn read_static(&self, _path: &str) -> Option<Result<Doc, SchemeError>> {
        None
    }
}

/// Read-only cross-extension view for the shared `letter://` resolver.
///
/// This is not `Services::records`, so the own-record boundary stays
/// intact: history entries are listed without reading another
/// extension's records.
pub trait LetterSourceIndex: Send + Sync + 'static {
    /// Lists history index lines for `session`, in display order.
    fn index_lines(&self, session: SessionId) -> BoxFuture<'_, Result<Vec<Box<str>>, SchemeError>>;
    /// Reads one history source by id for `session` as raw bytes.
    fn read<'a>(
        &'a self,
        session: SessionId,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Arc<[u8]>, SchemeError>>;
}

pub(crate) struct SchemeResolveContext<'a> {
    pub(crate) caller: &'a Caller,
    pub(crate) services: &'a Arc<dyn Services>,
    pub(crate) session: SessionId,
}

/// Host-minted context for one scheme-resolver invocation.
///
/// The host owns the caller identity, the session address, and the
/// runtime that backs [`SchemeCx::blob_get`] and [`SchemeCx::letter_index`].
/// Resolvers read through this context only; they never mint a [`Caller`]
/// or open the store directly.
pub struct SchemeCx<'a> {
    /// Host-minted caller; resolvers never forge one.
    pub caller: Caller,
    /// Capability-scoped service handle for this call.
    pub services: Arc<dyn Services>,
    /// Owning session (journal-pointer session).
    pub session: SessionId,
    rt: Arc<dyn SchemeCxRuntime>,
    /// The generation's doc table, shared with the host snapshot.
    docs: Arc<DocTable>,
    _marker: PhantomData<&'a ()>,
}

impl SchemeCx<'_> {
    /// Mints a resolver context; the host alone calls this constructor.
    pub(crate) fn new(
        caller: Caller,
        services: Arc<dyn Services>,
        session: SessionId,
        rt: Arc<dyn SchemeCxRuntime>,
        docs: Arc<DocTable>,
    ) -> Self {
        Self {
            caller,
            services,
            session,
            rt,
            docs,
            _marker: PhantomData,
        }
    }

    /// Borrows the host-minted caller needed for [`Services`] scoping.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// Borrows the capability-scoped service handle for this call.
    #[must_use]
    pub fn services(&self) -> &Arc<dyn Services> {
        &self.services
    }

    /// Returns the session this read runs against.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Looks one published doc page up by its full URI in this generation.
    #[must_use]
    pub fn doc(&self, uri: &str) -> Option<&DocPage> {
        self.docs.find(uri)
    }

    /// Lists every published doc page of this generation in URI order.
    #[must_use]
    pub fn docs(&self) -> &[DocPage] {
        self.docs.list()
    }

    /// Reads one content-addressed blob for the owning session.
    ///
    /// Returns `Ok(None)` for a missing blob so the resolver can map the
    /// absence to its own product text; store failures arrive as
    /// [`SchemeError`].
    #[must_use = "await it to read the blob"]
    pub fn blob_get<'b>(
        &'b self,
        id: &'b BlobId,
    ) -> BoxFuture<'b, Result<Option<Vec<u8>>, SchemeError>> {
        self.rt.blob_get(id)
    }

    /// Borrows the shared history view for `letter://` delegation.
    #[must_use]
    pub fn letter_index(&self) -> &dyn LetterSourceIndex {
        self.rt.letter_index()
    }
}

/// Host backing for [`SchemeCx`]; implemented once by the session actor.
///
/// This seam keeps all execution host-driven: resolvers cannot spawn tasks,
/// open channels, or block on the host thread. The runtime may offload
/// synchronous store reads; every method is a bounded, non-blocking read.
pub(crate) trait SchemeCxRuntime: Send + Sync + 'static {
    /// Reads one blob for the owning session; `None` means absent.
    fn blob_get<'a>(
        &'a self,
        id: &'a BlobId,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, SchemeError>>;
    /// Borrows the shared history view for `letter://` delegation.
    fn letter_index(&self) -> &dyn LetterSourceIndex;
}
