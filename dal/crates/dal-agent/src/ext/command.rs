//! Extension command runtime: handler trait and host-minted context.
//!
//! A slash command registers as a [`dal_core::CommandSpec`] plus an
//! [`CommandHandler`] pair via `ExtensionBuilder::command`; the spec's name
//! is a [`dal_core::CommandName`]. At dispatch the host parses the raw tail
//! into `args: &str`, mints one [`CommandCx`] for the call, and drives the
//! returned future to completion. Handlers never spawn threads, tasks, or
//! queues; background work goes through [`CommandCx::start_job`], which the
//! host runs in its own job table.
//!
//! [`CommandCx::leaf_entries`] returns the current leaf history with every
//! [`dal_core::JournalPart::TextBlob`] digest already hydrated to inline
//! text by the host. Prompt content carries [`dal_core::Expect`]; commands
//! the session cannot accept never reach a handler and surface as
//! [`dal_core::Rejection`] from the actor instead.

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::{
    Command, EntryView, JobId, JournalPart, ModelRoute, Origin, Page, PageReq, Reply, SessionId,
    SessionSummary, ThinkingLevel, TurnId, View,
};
use dal_provider::CatalogEntry;

use crate::error::ServiceError;

use super::generation::{Generation, ValidatedExtensions, splice_plugins};
use super::{BoxFuture, Caller, Extension, ScriptCx, Services};

/// The most leaf entries the host hands to one handler invocation.
///
/// The host truncates to this bound before constructing [`CommandCx`]; job
/// limits (one concurrent job per single-instance command name, sixteen
/// plugin jobs per session) stay with the host job table.
pub const MAX_LEAF_ENTRIES: usize = 1_024;

/// One slash-command implementation.
///
/// Registered alongside its [`dal_core::CommandSpec`]; the host calls
/// [`CommandHandler::run`] with the raw argument tail and a host-minted
/// context. Returning [`Reply::Job`] must follow a [`CommandCx::start_job`]
/// call for the same [`Command`]; returning [`Reply::Text`] replies inline.
pub trait CommandHandler: Send + Sync + 'static {
    /// Runs the command tail to a session [`Reply`].
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>>;
}

/// A [`CommandCx::submit_wait`] effect failed to persist.
///
/// Handlers match the variant; the built-in commands part renders the
/// product text through its own prose seam.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SaveError {
    /// The session closed before the effect could persist.
    #[error("session {id} is closed.")]
    SessionClosed {
        /// The closed session.
        id: SessionId,
    },
    /// The effect applied but the save failed with the product text.
    #[error("save failed: {message}")]
    Failed {
        /// The complete product text.
        message: Box<str>,
    },
}

/// A [`CommandCx::resolve_session`] lookup matched no session.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResolveMiss {
    /// Neither an id prefix nor a name matched in this workspace.
    #[error("no session named \"{query}\" in this workspace")]
    NotFound {
        /// The query that matched nothing.
        query: Box<str>,
    },
}

/// Cached provider model rows served to command handlers.
///
/// The host fills this from its catalog cache only; it never fetches.
/// An empty cache surfaces as [`None`] from [`CommandCx::catalog`], never
/// as an empty view.
#[derive(Clone, Debug)]
pub struct CatalogView {
    /// Cached rows in provider order.
    pub entries: Vec<CatalogEntry>,
}

/// Counts published by a successful plugin reload.
///
/// Defined here (not in `dal-ext`) so the host seam names it without a
/// dependency cycle; the built-in commands part imports it from this module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReloadOk {
    /// Plugins in the published generation.
    pub plugins: u64,
    /// Tools in the published generation.
    pub tools: u64,
    /// Commands in the published generation.
    pub commands: u64,
    /// Skills in the published generation.
    pub skills: u64,
}

/// Counts published by a successful [`CommandCx::publish_plugins`].
///
/// Narrower than [`ReloadOk`]: the plugin publication point counts only
/// user plugins and model-visible tools in the new generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReloadSummary {
    /// User-origin extensions in the published generation.
    pub plugins: u64,
    /// Tools in the published generation.
    pub tools: u64,
}

/// A plugin publication failure; nothing was published.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct BuildError {
    /// The complete product text.
    message: Box<str>,
}

impl BuildError {
    /// Wraps one validation failure without publishing.
    #[must_use]
    pub fn validation(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string().into(),
        }
    }
}

/// Host-minted context for one command-handler invocation.
///
/// The host owns the caller identity, the session address, the [`View`]
/// snapshot, and the [`CommandHost`] seam behind every accessor.
/// Handlers read through this context only; they never mint a [`Caller`]
pub struct CommandCx<'a> {
    caller: Caller,
    session: SessionId,
    turn: Option<TurnId>,
    services: Arc<dyn Services>,
    host: Arc<dyn CommandHost>,
    view: View,
    // Owned path copies minted via the `CommandHost` port at construction so
    // the `&Path` accessors below borrow from `self` (port returns `PathBuf`;
    // see port docs for why the seam is owned rather than lending `&Path`).
    data_root: PathBuf,
    session_file: Option<PathBuf>,
    log_path: PathBuf,
    script: Option<ScriptCx>,
    _marker: PhantomData<&'a ()>,
}

impl CommandCx<'_> {
    /// Mints a handler context; the host alone calls this constructor.
    ///
    /// The [`View`] snapshot is taken through the host seam at mint time so
    /// [`CommandCx::view`] borrows without further host calls. The owned
    /// path copies (`data_root`, `session_file`, `log_path`) are minted the
    /// same way so the `&Path` accessors borrow from `self`.
    pub(crate) fn new(
        caller: Caller,
        session: SessionId,
        turn: Option<TurnId>,
        services: Arc<dyn Services>,
        host: Arc<dyn CommandHost>,
    ) -> Self {
        let view = host.view(&caller, session, turn);
        let data_root = host.data_root(&caller, session, turn);
        let session_file = host.session_file(&caller, session, turn);
        let log_path = host.log_path(&caller, session, turn);
        Self {
            caller,
            session,
            turn,
            services,
            host,
            view,
            data_root,
            session_file,
            log_path,
            script: None,
            _marker: PhantomData,
        }
    }

    /// Borrows the host-minted caller needed for [`Services`] scoping.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// Attaches the script context of a scripted command.
    #[must_use]
    pub fn with_script(mut self, script: ScriptCx) -> Self {
        self.script = Some(script);
        self
    }

    /// Borrows the script context, present only for scripted commands.
    #[must_use]
    pub fn script(&self) -> Option<&ScriptCx> {
        self.script.as_ref()
    }

    /// Returns the session this command runs against.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the owning turn when the dispatch captured one.
    #[must_use]
    pub fn turn(&self) -> Option<TurnId> {
        self.turn
    }

    /// Borrows the capability-scoped service handle for this call.
    #[must_use]
    pub fn services(&self) -> &Arc<dyn Services> {
        &self.services
    }

    /// Borrows the session snapshot taken when the host minted this context.
    #[must_use]
    pub fn view(&self) -> &View {
        &self.view
    }

    /// Borrows the host data directory captured when the host minted this context.
    #[must_use]
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Borrows the session journal file captured at mint time, or [`None`]
    /// when the session is memory-only.
    #[must_use]
    pub fn session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    /// Borrows the session log file captured when the host minted this context.
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Submits `command` as this caller and waits for it to persist.
    ///
    /// The future resolves once the effect is durable; it never blocks a
    /// thread while waiting.
    #[must_use]
    pub fn submit_wait(&self, command: Command) -> BoxFuture<'_, Result<(), SaveError>> {
        self.host
            .submit_wait(&self.caller, self.session, self.turn, command)
    }

    /// Starts a host-driven background job for `command`.
    ///
    /// The call only enqueues; the host job table runs the command without
    /// holding the dispatch queue and delivers its outcome exactly once.
    #[must_use]
    pub fn start_job(&self, command: Command) -> JobId {
        self.host
            .start_job(&self.caller, self.session, self.turn, command)
    }

    /// Cancels the owning turn through the host control cell, when one runs.
    pub fn cancel_turn(&self) {
        self.host.cancel_turn(&self.caller, self.session, self.turn);
    }

    /// Lists sessions in this workspace: at most `limit` rows (`0` takes the
    /// host default), after the opaque `cursor`, filtered by `search`.
    ///
    /// Rows arrive newest first; `next_before` names the following page.
    #[must_use]
    pub fn sessions_page(
        &self,
        limit: u32,
        cursor: Option<&str>,
        search: Option<&str>,
    ) -> Page<SessionSummary, Box<str>> {
        self.host
            .sessions_page(&self.caller, self.session, self.turn, limit, cursor, search)
    }

    /// Resolves an id prefix or name to a session in this workspace.
    ///
    /// The host tries an id prefix first, then a name. The client opens
    /// `SessionRef::Resume` from `summary.id`.
    ///
    /// # Errors
    /// Returns [`ResolveMiss`] when no session matches `query`.
    pub fn resolve_session(&self, query: &str) -> Result<SessionSummary, ResolveMiss> {
        self.host
            .resolve_session(&self.caller, self.session, self.turn, query)
    }

    /// Resolves a model alias or id to a provider route from cache only.
    ///
    /// The host consults the cached provider catalog plus config aliases;
    /// like [`CommandCx::resolve_session`] this never fetches.
    ///
    /// # Errors
    /// Returns [`ResolveMiss`] when no model matches `query`.
    pub fn resolve_model(&self, query: &str) -> Result<ModelRoute, ResolveMiss> {
        self.host
            .resolve_model(&self.caller, self.session, self.turn, query)
    }

    /// Returns the cached provider catalog, or [`None`] when the host has
    /// not fetched one yet. Never triggers a fetch.
    #[must_use]
    pub fn catalog(&self) -> Option<CatalogView> {
        self.host.catalog(&self.caller, self.session, self.turn)
    }

    /// Returns the thinking levels the current route supports.
    #[must_use]
    pub fn levels_for(&self) -> Vec<ThinkingLevel> {
        self.host.levels_for(&self.caller, self.session, self.turn)
    }

    /// Returns stored credentials as `(provider, kind)` pairs in stable
    /// provider order, without exposing secret contents.
    #[must_use]
    pub fn auth_stored(&self) -> Vec<(Box<str>, Box<str>)> {
        self.host.auth_stored(&self.caller, self.session, self.turn)
    }

    /// Removes the stored credential for `provider`; returns whether one
    /// existed.
    #[must_use]
    pub fn auth_remove(&self, provider: &str) -> bool {
        self.host
            .auth_remove(&self.caller, self.session, self.turn, provider)
    }

    /// Returns the embedded docs page for `uri`. Unknown URIs yield an
    /// empty page; a missing page is a build-time failure, never a runtime
    /// error here.
    #[must_use]
    pub fn docs_page(&self, uri: &str) -> Box<str> {
        self.host
            .docs_page(&self.caller, self.session, self.turn, uri)
    }

    /// Returns the changelog page URI served by [`CommandCx::docs_page`].
    #[must_use]
    pub fn changelog_uri(&self) -> &'static str {
        self.host
            .changelog_uri(&self.caller, self.session, self.turn)
    }

    /// Returns the edit style the patch tool applies for `route`.
    #[must_use]
    pub fn edit_style_for(&self, route: &ModelRoute) -> Box<str> {
        self.host
            .edit_style_for(&self.caller, self.session, self.turn, route)
    }

    /// Returns the current-leaf history with text blobs hydrated.
    ///
    /// Every [`JournalPart::TextBlob`] has been resolved to inline text by
    /// the host; the returned vector holds at most [`MAX_LEAF_ENTRIES`]
    /// entries in chronological order.
    #[must_use]
    pub fn leaf_entries(&self) -> Vec<EntryView> {
        self.host
            .leaf_entries(&self.caller, self.session, self.turn)
    }

    /// Reads one page of full history in leaf order for export.
    ///
    /// Unlike [`CommandCx::leaf_entries`], pages walk the whole branch
    /// through `query`; the host hydrates text blobs as it does for leaves.
    #[must_use]
    pub fn history_page(
        &self,
        query: PageReq,
    ) -> BoxFuture<'_, Result<Page<EntryView>, ServiceError>> {
        self.host
            .history_page(&self.caller, self.session, self.turn, query)
    }

    /// Validates and publishes new plugin extensions against the current
    /// product set: the one publication point for `/reload`.
    ///
    /// Takes the current generation's `Builtin` and `Bundled` extensions,
    /// appends `plugins`, validates the complete set standalone, builds
    /// the new generation, and publishes it through the host-owned watch
    /// sender. A validation failure publishes nothing, so the old
    /// generation stays live; a turn that started before the publish
    /// keeps dispatching on its captured generation.
    #[must_use]
    pub fn publish_plugins(
        &self,
        plugins: Vec<Extension>,
    ) -> BoxFuture<'_, Result<ReloadSummary, BuildError>> {
        let current = self
            .host
            .current_extensions(&self.caller, self.session, self.turn);
        let extensions = splice_plugins(&current, plugins);
        let validated = ValidatedExtensions::validate(extensions, None);
        Box::pin(async move {
            let generation = Generation::build(validated.map_err(BuildError::validation)?);
            let summary = ReloadSummary {
                plugins: generation
                    .extensions
                    .iter()
                    .filter(|ext| ext.origin() == Origin::User)
                    .count() as u64,
                tools: generation.tools.entries().len() as u64,
            };
            self.host
                .publish_generation(&self.caller, self.session, self.turn, generation);
            Ok(summary)
        })
    }
}

/// Host backing for [`CommandCx`]; implemented once by the session actor.
///
/// The single seam between handlers and the host: every accessor delegates
/// here with the caller, session, and turn passed explicitly. The trait
/// performs no spawning, no channels, and no blocking waits of its own;
/// waiting surfaces as futures the host resolves.
pub(crate) trait CommandHost: Send + Sync + 'static {
    /// Snapshots the session view minted into [`CommandCx`].
    fn view(&self, caller: &Caller, session: SessionId, turn: Option<TurnId>) -> View;
    /// Returns the host data directory as an owned path.
    ///
    /// Owned (`PathBuf`) so [`CommandCx`] can mint one copy at construction
    /// and lend `&Path` from `self`: a `&Path`-returning port would tie the
    /// borrow to the host object and require every host to lend stably.
    fn data_root(&self, caller: &Caller, session: SessionId, turn: Option<TurnId>) -> PathBuf;
    /// Returns the session journal file, or [`None`] when memory-only.
    ///
    /// Owned for the same mint-and-borrow reason as [`CommandHost::data_root`].
    fn session_file(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Option<PathBuf>;
    /// Returns the session log file as an owned path.
    ///
    /// Owned for the same mint-and-borrow reason as [`CommandHost::data_root`].
    fn log_path(&self, caller: &Caller, session: SessionId, turn: Option<TurnId>) -> PathBuf;
    /// Applies `command` as the caller and resolves once it is durable.
    fn submit_wait(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        command: Command,
    ) -> BoxFuture<'_, Result<(), SaveError>>;
    /// Enqueues `command` as a background job and returns its identity.
    fn start_job(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        command: Command,
    ) -> JobId;
    /// Cancels the owning turn through the control cell, when one runs.
    fn cancel_turn(&self, caller: &Caller, session: SessionId, turn: Option<TurnId>);
    /// Lists sessions newest first with an opaque text cursor.
    fn sessions_page(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        limit: u32,
        cursor: Option<&str>,
        search: Option<&str>,
    ) -> Page<SessionSummary, Box<str>>;
    /// Resolves an id prefix (first) or name (then) in this workspace.
    fn resolve_session(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        query: &str,
    ) -> Result<SessionSummary, ResolveMiss>;
    /// Resolves a model alias or id to a route from cache only (catalog plus
    /// config aliases); never fetches. Sync like `resolve_session`.
    fn resolve_model(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        query: &str,
    ) -> Result<ModelRoute, ResolveMiss>;
    /// Reads the cached provider catalog without fetching.
    fn catalog(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Option<CatalogView>;
    /// Reads the thinking levels the current route supports.
    fn levels_for(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Vec<ThinkingLevel>;
    /// Lists stored credentials as `(provider, kind)` pairs, secret-free.
    fn auth_stored(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Vec<(Box<str>, Box<str>)>;
    /// Removes one stored credential; returns whether one existed.
    fn auth_remove(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        provider: &str,
    ) -> bool;
    /// Reads one embedded docs page.
    fn docs_page(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        uri: &str,
    ) -> Box<str>;
    /// Returns the changelog page URI served by the docs pages.
    fn changelog_uri(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> &'static str;
    /// Reads the edit style the patch tool applies for `route`.
    fn edit_style_for(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        route: &ModelRoute,
    ) -> Box<str>;
    /// Reads the current-leaf history with text blobs hydrated.
    fn leaf_entries(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Vec<EntryView>;
    /// Reads one page of full history in leaf order for export.
    fn history_page(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        query: PageReq,
    ) -> BoxFuture<'_, Result<Page<EntryView>, ServiceError>>;
    /// Snapshots the current generation's extensions in canonical order
    /// for [`CommandCx::publish_plugins`].
    fn current_extensions(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
    ) -> Vec<Extension>;
    /// Publishes one validated generation through the host-owned watch
    /// sender for [`CommandCx::publish_plugins`]. Only validated
    /// generations arrive here; watch send failures (no listener) are
    /// the host's to absorb.
    fn publish_generation(
        &self,
        caller: &Caller,
        session: SessionId,
        turn: Option<TurnId>,
        generation: Generation,
    );
}

/// Reports whether any visible part still names an unhydrated text blob.
///
/// The host guarantees [`CommandCx::leaf_entries`] returns false here:
/// every [`JournalPart::TextBlob`] is hydrated to inline text before the
/// handler runs. Image and opaque blobs keep their digests by design.
#[must_use]
pub fn has_unhydrated_text_blob(entries: &[EntryView]) -> bool {
    for entry in entries {
        let parts = match &entry.kind {
            dal_core::EntryKind::User { parts } | dal_core::EntryKind::ToolResult { parts, .. } => {
                parts.as_slice()
            }
            _ => continue,
        };
        if parts
            .iter()
            .any(|part| matches!(part, JournalPart::TextBlob { .. }))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
