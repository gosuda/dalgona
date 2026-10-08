//! Extension runtime surface: builder, records, hooks, and model plumbing.
//!
//! Record and value types come from `dal_core`; this module owns the
//! runtime traits and the `ExtensionBuilder`/`Extension` pair. Host-wired
//! behavior (grants, services, dispatch, generation) lands in `ext/*`.

use std::num::NonZeroU32;

use crate::Env;

mod builder;
pub mod command;
pub mod compact;
pub mod docs;
pub mod generation;
pub mod grants;
pub mod hooks;
pub mod mcp;
pub(crate) mod overlay;
pub mod prompt;
pub mod scheme;
pub mod scope;
pub mod script;
pub mod services;
pub(crate) mod synthetic;
pub mod tool;

pub use builder::{Extension, ExtensionBuilder};
pub use command::{CommandCx, CommandHandler};
pub use compact::{
    CompactError, CompactInput, Compaction, Compactor, CoveredEntry, ImageProfile, Replacement,
    SUMMARY_MAX_OUTPUT_TOKENS,
};
use dal_core::ext::{ExportId, OpSet};
pub use docs::DocRecord;
pub use generation::catalog::{Catalog, ExportSpec, OpSpec, wire_name};
pub use hooks::{
    GateSeed, StreamFire, StreamFireAction, StreamWatch, StreamWatchRecord, TurnInfo, WatchBudget,
    WatchFactory,
};
pub use mcp::McpClient;
pub use prompt::{PromptOrder, PromptSection, SectionCx, SectionFn};
pub use scheme::{Doc, LetterSourceIndex, SchemeCx, SchemeResolver};
pub use scope::{Scope, ScopeError, ScopeHandle, ScopeHandleId, ScopeStatus, ScopeValue};
pub use script::{
    AdoptError, CancelTarget, Cleanup, Collect, Collected, EffectGate, EffectStatus, Entry,
    EvalEnvironment, Evidence, FailureCode, HostTerminal, Invocation, InvocationId, ObserverError,
    OpFailure, OpOutcome, OpRecord, OpRequest, OpValue, Parent, ScopeId, ScriptCx, ScriptHost,
    Submit, TaskId, WorkerPermit,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
pub use tool::{Approved, ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};

use dal_core::{
    Caps, Inference, ModelId, ModelRequest, Name, Origin, RawJson, ScopeSpec, ServiceSet,
    SessionId, TurnId,
};
pub use dal_provider::EventStream;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A boxed sendable future used by hook and handler signatures.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub use services::Services;

/// Host-minted identity for one extension entry point.
#[derive(Clone, Debug)]
pub struct Caller {
    ext: Name,
    origin: Origin,
    inject: ServiceSet,
    state_version: NonZeroU32,
    kind: CallerKind,
    turn: Option<TurnId>,
}

/// How a [`Caller`] was minted.
#[derive(Clone, Debug)]
pub enum CallerKind {
    /// A command or model handler.
    Handler,
    /// A lifecycle or guarding hook.
    Hook,
    /// An eval cell.
    Cell {
        /// Whether the cell was approved.
        approved: bool,
    },
    /// A tool call.
    Tool,
}

impl Caller {
    pub(crate) fn new(
        ext: Name,
        origin: Origin,
        inject: ServiceSet,
        state_version: NonZeroU32,
        kind: CallerKind,
        turn: Option<TurnId>,
    ) -> Self {
        Self {
            ext,
            origin,
            inject,
            state_version,
            kind,
            turn,
        }
    }

    /// Borrows the owning extension name.
    ///
    /// A [`Services`] implementation reads this to scope a call to the
    /// extension that made it.
    #[must_use]
    pub fn ext(&self) -> &Name {
        &self.ext
    }

    /// The `state_version` the caller's extension declared when the
    /// caller was minted (R08). The mint site captures the generation
    /// snapshot, so an in-flight invocation keeps its own namespace
    /// across a plugin reload. Built-in and synthetic callers mint
    /// [`NonZeroU32::MIN`].
    #[must_use]
    pub(crate) fn state_version(&self) -> NonZeroU32 {
        self.state_version
    }

    /// The origin class of the extension that made this caller.
    #[must_use]
    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }

    pub(crate) fn turn(&self) -> Option<TurnId> {
        self.turn
    }

    /// Reports whether this caller is an approved eval cell.
    ///
    /// A cell the ladder approved may run its nested host operations
    /// without asking again: one ask per capability cell.
    #[must_use]
    pub(crate) fn cell_approved(&self) -> bool {
        matches!(self.kind, CallerKind::Cell { approved: true })
    }

    /// Reports whether this caller is an eval cell, approved or not.
    #[must_use]
    pub(crate) fn cell(&self) -> bool {
        matches!(self.kind, CallerKind::Cell { .. })
    }
}

/// A hook failure; cancellation stays distinct from error text.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum HookError {
    /// The hook failed with display text.
    #[error("{message}")]
    Failed {
        /// The failure text.
        message: Box<str>,
    },
    /// The hook was cancelled.
    #[error("hook cancelled")]
    Cancelled,
}

/// A hook returning a verdict or value.
pub trait Hook<I, O>: Send + Sync + 'static {
    /// Runs the hook on `input`.
    fn call(&self, input: I, cx: HookCx) -> BoxFuture<'static, Result<O, HookError>>;
}

/// A hook observing an event without a verdict.
pub trait ObserveHook<I>: Send + Sync + 'static {
    /// Observes `input`.
    fn call(&self, input: I, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>>;
}

/// Context minted per extension for one hook invocation.
pub struct HookCx {
    /// The host-minted caller.
    pub caller: Caller,
    /// The capability-scoped service handle.
    pub services: Arc<dyn Services>,
    /// The host-captured process-edge environment snapshot.
    process_env: Arc<Env>,
    /// The owning session.
    pub session: SessionId,
    /// The parent session when the hook runs in a subagent session.
    pub parent: Option<SessionId>,
    /// The owning turn, when the event has one.
    pub turn: Option<TurnId>,
    /// The hook's cancellation token.
    pub cancel: CancellationToken,
    /// The wall deadline shared by the dispatch chain.
    pub deadline: Instant,
    /// The script context of a scripted hook, absent for Rust hooks.
    pub script: Option<ScriptCx>,
}

/// A controller the host starts once at startup with a clone of itself; the
/// host tracks the returned future and aborts it at shutdown.
pub type Attach = Box<dyn FnOnce(crate::Host) -> BoxFuture<'static, ()> + Send>;

/// Leaf-aware status poll; the host owns the wait loop.
pub trait StatusPoll: Send + Sync + 'static {
    /// Takes one status snapshot.
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot;
}

/// Host-provided synchronous context for one status poll: the polling
/// session plus a borrowed view of the calling extension's current-leaf
/// records grouped by kind. The borrow ties the view to the actor fold,
/// so records cannot go stale across a leaf move.
pub struct StatusCx<'a> {
    /// The polling session.
    pub session: SessionId,
    records: &'a BTreeMap<Box<str>, Vec<ExtRecord>>,
}

impl<'a> StatusCx<'a> {
    /// Builds one poll context over `session` and the grouped records view.
    pub(crate) fn new(session: SessionId, records: &'a BTreeMap<Box<str>, Vec<ExtRecord>>) -> Self {
        Self { session, records }
    }
}

impl StatusCx<'_> {
    /// Returns the calling extension's current-leaf records of one kind.
    #[must_use]
    pub fn records(&self, kind: &str) -> &[ExtRecord] {
        self.records.get(kind).map_or(&[], Vec::as_slice)
    }
}

/// One leaf record contributed by the calling extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtRecord {
    /// The contributing extension.
    pub ext: Name,
    /// The record kind.
    pub kind: Box<str>,
    /// The record body.
    pub body: RawJson,
}

/// Leaf-aware payload for one status poll; `quiet` keeps `is_quiet` semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusSnapshot {
    /// Whether the status is quiet.
    pub quiet: bool,
    /// The status text, when any.
    pub text: Option<Box<str>>,
}

/// One registered status kind with its poll.
pub struct StatusRecord {
    /// The status kind.
    pub kind: Box<str>,
    /// The kind's poll.
    pub poll: Arc<dyn StatusPoll>,
}

/// One registered model with its capability summary and handler.
#[derive(Clone)]
pub struct ModelRecord {
    /// The model identity.
    pub id: ModelId,
    /// The capability summary.
    pub caps: Caps,
    /// The inference handler.
    pub handler: Arc<dyn ModelHandler>,
    /// The Starlark export identity, absent for Rust-registered models.
    pub export: Option<ExportId>,
}

/// Runs inference for one registered model.
pub trait ModelHandler: Send + Sync + 'static {
    /// The declared operations of a scripted model handler.
    #[must_use]
    fn uses(&self) -> OpSet {
        OpSet::EMPTY
    }

    /// Runs inference for `request`.
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>>;
}

/// Scoped-inference context minted by the host.
pub struct ModelCx<'a> {
    rt: Arc<dyn ModelCxRuntime>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl ModelCx<'_> {
    /// Builds a model context over one runtime.
    pub(crate) fn new(rt: Arc<dyn ModelCxRuntime>) -> Self {
        Self {
            rt,
            _marker: std::marker::PhantomData,
        }
    }

    /// Opens a fan-out scope whose handles this run owns.
    ///
    /// # Errors
    /// Returns [`ScopeError::Spec`] for an invalid limit or budget.
    pub fn scope(&self, spec: ScopeSpec) -> Result<Scope, ScopeError> {
        self.rt.scope(spec)
    }

    /// Returns the script context for this model handler, when scripted.
    #[must_use]
    pub fn script_cx(&self) -> Option<ScriptCx> {
        self.rt.script_cx(Arc::clone(&self.rt))
    }

    /// Forwards `request` once, with `private` tools visible only to it.
    #[must_use]
    pub fn forward<'a>(
        &'a self,
        request: ModelRequest,
        private: &'a [PrivateTool],
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.rt.forward(request, private)
    }
}

pub(crate) trait ModelCxRuntime: Send + Sync + 'static {
    fn scope(&self, spec: ScopeSpec) -> Result<Scope, ScopeError>;
    fn script_cx(&self, _runtime: Arc<dyn ModelCxRuntime>) -> Option<ScriptCx> {
        None
    }
    fn infer<'a>(
        &'a self,
        who: &'a Caller,
        request: ModelRequest,
        script: Arc<crate::session::script::SessionScriptHost>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Inference, crate::error::ServiceError>>;
    fn forward<'a>(
        &'a self,
        request: ModelRequest,
        private: &'a [PrivateTool],
    ) -> BoxFuture<'a, Result<EventStream, ModelError>>;
}

/// Model-context failures.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    /// `forward` was called a second time.
    #[error("forward may be called at most once")]
    SecondForward,
    /// The private tool rounds ran out.
    #[error("private tool rounds exhausted")]
    PrivateRounds,
    /// A synthetic model route reached itself.
    #[error("synthetic model cycle")]
    SyntheticCycle {
        /// The route chain that closed the cycle.
        chain: Vec<ModelId>,
    },
    /// A synthetic model route nested too deep.
    #[error("synthetic model depth exceeded")]
    SyntheticDepth {
        /// The route chain at the limit.
        chain: Vec<ModelId>,
    },
}

/// A private tool visible only to one forwarded request.
#[derive(Clone)]
pub struct PrivateTool(pub Arc<dyn Tool>);

#[cfg(test)]
mod tests;

/// Model-visible tool description for prompt sections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDescription {
    /// The tool name.
    pub name: Name,
    /// The model-facing description.
    pub description: Box<str>,
}

impl HookCx {
    /// Returns the process-edge environment snapshot captured by the host.
    #[must_use]
    pub fn process_env(&self) -> Arc<Env> {
        Arc::clone(&self.process_env)
    }

    /// Mints a test context over `services` with an internal Builtin caller.
    /// Foreign test batteries open handles through this context; the host
    /// never uses it.
    #[must_use]
    pub fn for_test(services: Arc<dyn Services>, session: SessionId, turn: Option<TurnId>) -> Self {
        Self {
            parent: None,
            caller: Caller::new(
                Name::test(),
                Origin::Builtin,
                ServiceSet::EMPTY,
                NonZeroU32::MIN,
                CallerKind::Hook,
                turn,
            ),
            services,
            process_env: Arc::new(Env {
                vars: BTreeMap::default(),
                cwd: PathBuf::default(),
                sandbox_helper: None,
            }),
            session,
            turn,
            cancel: CancellationToken::new(),
            deadline: Instant::now() + std::time::Duration::from_secs(60),
            script: None,
        }
    }
}
