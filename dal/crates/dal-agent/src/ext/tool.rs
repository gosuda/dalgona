//! The §8.4 tool surface: identity, calls, outcomes, approval proofs,
//! and the host-minted tool context.
//!
//! [`Tool`] is the full runtime trait; it replaces the empty stub at
//! integration. The host mints one [`ToolCx`] per dispatched call and one
//! move-only [`Approved`] per granted authorization. [`ToolCx::spawn`]
//! consumes the proof exactly once and [`ToolCx::detach`] parks the child
//! as a background job. [`SpawnOpts`] and [`Proc`] live in [`crate::proc`]
//! (alongside `ProcResult`, which no §8.4 signature needs); this module
//! reuses them without redefining them.
use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::ext::{Consumer, ToolData};
use dal_core::{
    CallId, DenyReason, GenerationId, JobId, ModelInfo, Name, Origin, Part, Preview, RawJson,
    ServiceSet, SessionId, ToolClass, ToolSpec, TurnId, Workspace,
};
use tokio_util::sync::CancellationToken;

use super::scheme::SchemeResolveContext;
use super::{BoxFuture, Caller, CallerKind, Doc, ScriptCx, Services, ToolDescription};
use crate::Env;
use crate::error::{SchemeError, ToolError};
use crate::proc::{Proc, SpawnOpts};

/// Raw tool arguments preserved byte for byte.
///
/// This is the core [`RawJson`] carrier under the name the tool surface
/// uses, so `&RawValue` and `&RawJson` are the same type and classifiers
/// written against either spelling agree.
pub type RawValue = RawJson;

/// A tool-argument decode failure; the display text is the exact
/// model-visible string.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ArgError {
    message: Box<str>,
}

impl ArgError {
    /// Builds an argument error with exact model-visible text.
    #[must_use]
    pub fn message(text: impl Into<Box<str>>) -> Self {
        Self {
            message: text.into(),
        }
    }
}

/// One dispatched tool invocation: the call identity plus its raw arguments.
///
/// The checked tool name travels alongside the call in the dispatch map and
/// the `ToolCallEvent` hook payload, never inside the call itself.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// The provider's call identity, journaled alongside the result.
    pub id: CallId,
    /// The final arguments after `tool_call` hooks and approval.
    pub args: RawValue,
}

impl ToolCall {
    /// Builds a call from its identity text and raw arguments.
    #[must_use]
    pub fn new(id: impl Into<Box<str>>, args: RawValue) -> Self {
        Self {
            id: CallId::new(id),
            args,
        }
    }
}

/// A successful tool result: content parts plus paths the call changed.
#[derive(Clone, Debug, Default)]
pub struct ToolOutput {
    /// The result content; text parts render, blob parts ride the store.
    pub parts: Vec<Part>,
    /// Canonical paths this call wrote; empty unless the tool reports writes.
    pub files_changed: Vec<PathBuf>,
    /// Typed data beside the display text, such as read and search views (R06).
    pub data: Option<ToolData>,
}

impl ToolOutput {
    /// Builds output from finished content parts; nothing is marked changed.
    #[must_use]
    pub fn new(parts: Vec<Part>) -> Self {
        Self {
            parts,
            files_changed: Vec::new(),
            data: None,
        }
    }

    /// Builds text-only output from one string.
    #[must_use]
    pub fn from_text(text: impl Into<Box<str>>) -> Self {
        Self {
            parts: vec![Part::Text { text: text.into() }],
            files_changed: Vec::new(),
            data: None,
        }
    }
}
impl fmt::Display for ToolOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let texts = self.parts.iter().filter_map(|part| match part {
            Part::Text { text } => Some(text),
            Part::Image { .. } | Part::Blob { .. } => None,
        });
        let mut first = true;
        for text in texts {
            if !first {
                formatter.write_str("\n")?;
            }
            first = false;
            formatter.write_str(text)?;
        }
        Ok(())
    }
}

/// The terminal result of one tool run.
#[derive(Debug)]
pub enum ToolOutcome {
    /// The tool returned successfully.
    Ok(Box<ToolOutput>),
    /// The tool returned an error.
    Err(ToolError),
    /// Cancellation arrived before the tool settled.
    Interrupted,
    /// The foreground budget expired; the child continues as this job.
    Detached(JobId),
}
/// Private proof seal: only [`ToolCx::authorize`] mints [`Approved`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Seal;

/// Move-only authorization proof for one spawn.
///
/// The fields stay private so extensions cannot forge or alter a grant:
/// the host checks `argv` against `prefix`, every write path against
/// `roots`, and revokes a job-scoped grant when its job ends.
/// [`ToolCx::spawn`] consumes the proof, so one approval spawns at most
/// once. The type is deliberately not `Clone`.
#[derive(Debug)]
pub struct Approved {
    call: CallId,
    digest: Option<[u8; 32]>,
    prefix: Box<[OsString]>,
    roots: Box<[PathBuf]>,
    job: Option<JobId>,
    _seal: Seal,
}

impl Approved {
    /// Mints a proof; authorization alone calls this constructor.
    #[must_use]
    pub(crate) fn new(
        call: CallId,
        digest: Option<[u8; 32]>,
        prefix: Box<[OsString]>,
        roots: Box<[PathBuf]>,
        job: Option<JobId>,
    ) -> Self {
        Self {
            call,
            digest,
            prefix,
            roots,
            job,
            _seal: Seal,
        }
    }

    /// Borrows the call this proof authorizes.
    #[must_use]
    pub(crate) fn call(&self) -> &CallId {
        &self.call
    }

    /// Borrows the preview digest this proof binds, when the preview carried one.
    #[must_use]
    pub(crate) fn digest(&self) -> Option<[u8; 32]> {
        self.digest
    }

    /// Borrows the argv prefix the spawn must start with.
    #[must_use]
    pub(crate) fn prefix(&self) -> &[OsString] {
        &self.prefix
    }

    /// Borrows the canonical roots writes must stay inside.
    #[must_use]
    pub(crate) fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns the job whose end revokes the grant, when this is job-scoped.
    #[must_use]
    pub(crate) fn job(&self) -> Option<JobId> {
        self.job
    }
}

/// One host-registered tool: identity, per-model schema, classification, run.
pub trait Tool: Send + Sync + 'static {
    /// Returns the extension that declared the tool's external capability
    /// owner, if different from the extension registering this tool.
    ///
    /// Session tools such as mapped MCP tools use this to retain the
    /// declaring plugin's caller identity after the tool enters the overlay.
    fn declaring_extension(&self) -> Option<Name> {
        None
    }

    /// Returns the registered tool name.
    fn name(&self) -> &Name;
    /// Returns the schema for one model; byte-stable per (tool, model).
    fn spec(&self, model: &ModelInfo) -> Arc<ToolSpec>;
    /// Classifies the final arguments; `Err` is model-visible text, never
    /// a ladder bypass.
    ///
    /// # Errors
    /// Returns [`ArgError`] for arguments the tool cannot classify.
    fn classify(&self, args: &RawValue, ws: &Workspace) -> Result<ToolClass, ArgError>;
    /// Runs the call to one terminal outcome.
    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome>;
    /// Builds the model-visible identity of this tool for one model.
    ///
    /// The default body reads [`Tool::name`] and [`Tool::spec`]; the prompt
    /// projection calls this so names and descriptions stay byte-identical
    /// to the registered specs.
    fn identity(&self, model: &ModelInfo) -> ToolDescription {
        let spec = self.spec(model);
        ToolDescription {
            name: self.name().clone(),
            description: spec.description.clone(),
        }
    }
}

/// Maximum lines retained in one tool output sink; older lines shed first.
pub const OUTPUT_SINK_CAPACITY: usize = 256;
/// Maximum bytes kept per sunk line; longer lines cut at a char boundary.
pub const OUTPUT_SINK_LINE_BYTES: usize = 4_096;

/// Bounded foreground-output buffer owned by one [`ToolCx`].
///
/// The queue sheds the oldest line under flood and counts the drops, so a
/// verbose child cannot grow the turn task without bound.
#[derive(Debug, Default)]
pub struct OutputSink {
    lines: VecDeque<Box<str>>,
    dropped: usize,
}

impl OutputSink {
    /// Builds an empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one line, truncating to [`OUTPUT_SINK_LINE_BYTES`] bytes and
    /// shedding the oldest line past [`OUTPUT_SINK_CAPACITY`].
    pub fn push(&mut self, line: &str) {
        let kept: Box<str> = floor_char_boundary(line, OUTPUT_SINK_LINE_BYTES).into();
        if self.lines.len() >= OUTPUT_SINK_CAPACITY {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(kept);
    }

    /// Returns the number of retained lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Reports whether the sink holds no lines.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Returns the number of lines shed under flood.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// Borrows the retained lines oldest first.
    #[must_use]
    pub fn lines(&self) -> VecDequeIter<'_, Box<str>> {
        self.lines.iter()
    }
}

/// Borrowed line iterator for [`OutputSink::lines`].
pub type VecDequeIter<'a, T> = std::collections::vec_deque::Iter<'a, T>;

/// Cuts `line` to at most `max` bytes without splitting a character.
fn floor_char_boundary(line: &str, max: usize) -> &str {
    if line.len() <= max {
        return line;
    }
    let mut end = max;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// Host backing for [`ToolCx`]; implemented once by the session actor.
///
/// This seam keeps all execution host-driven: the trait performs no
/// spawning, no channels, and no blocking waits of its own.
pub(crate) trait ToolCxRuntime: Send + Sync + 'static {
    /// Runs the approval ladder for `call` over `preview`.
    fn authorize(
        &self,
        call: &CallId,
        preview: Preview,
        cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, DenyReason>>;
    /// Launches one checked child; consumes the approval proof.
    fn spawn(
        &self,
        argv: &[OsString],
        opts: SpawnOpts,
        approved: Approved,
    ) -> Result<Proc, ToolError>;
    /// Parks a spawned child as a background job and returns its identity.
    fn detach(&self, proc: Proc) -> JobId;
    /// Resolves one `scheme://` URI to its page text.
    fn resolve(
        &self,
        uri: &str,
        context: SchemeResolveContext<'_>,
    ) -> BoxFuture<'_, Result<Doc, ToolError>>;
    /// Borrows the session workspace spawned children resolve against.
    fn workspace(&self) -> &Workspace;
    /// Borrows the cancellation token bounding this call.
    fn cancel(&self) -> &CancellationToken;
}

/// The host state one tool call captures at minting.
pub(crate) struct CallSnapshot {
    /// The catalog generation the call runs against.
    pub(crate) generation: GenerationId,
    /// The frozen parent observation cutoff.
    pub(crate) cutoff: Option<u64>,
    /// The host's process-edge snapshot.
    pub(crate) env: Arc<Env>,
}

/// What one private tool call inherits: no approval front end and no process
/// launcher exist, so authorization and spawning deny.
#[derive(Clone)]
pub(crate) struct PrivateCx {
    /// The model handler's identity.
    pub(crate) caller: Caller,
    /// The owning session, or a fresh id outside a session.
    pub(crate) session: SessionId,
    /// The services the private tool reaches.
    pub(crate) services: Arc<dyn Services>,
    /// The workspace private tools resolve against.
    pub(crate) workspace: Workspace,
    /// The host's process-edge snapshot.
    pub(crate) env: Arc<Env>,
    /// The generation the run started under.
    pub(crate) generation: GenerationId,
    /// The run's cancellation token.
    pub(crate) cancel: CancellationToken,
}

impl PrivateCx {
    /// Mints the context of one private tool call.
    pub(crate) fn mint(&self, call: CallId) -> ToolCx<'static> {
        ToolCx::new(
            self.caller.clone(),
            call,
            self.session,
            None,
            Arc::clone(&self.services),
            Arc::new(ForTestRuntime {
                workspace: self.workspace.clone(),
                cancel: self.cancel.clone(),
                approve: false,
            }),
            CallSnapshot {
                generation: self.generation,
                cutoff: None,
                env: Arc::clone(&self.env),
            },
        )
    }
}

/// Host-minted context for one tool-call invocation.
///
/// Tools read through this context only; they never mint a [`Caller`],
/// touch the journal, or spawn outside [`ToolCx::spawn`]. The `caller`
/// behind [`ToolCx::services`] carries `kind: Tool`.
pub struct ToolCx<'a> {
    caller: Caller,
    generation: GenerationId,
    cutoff: Option<u64>,
    call: CallId,
    session: SessionId,
    turn: Option<TurnId>,
    services: Arc<dyn Services>,
    output: OutputSink,
    rt: Arc<dyn ToolCxRuntime>,
    consumer: Consumer,
    env: Arc<Env>,
    script: Option<ScriptCx>,
    _marker: PhantomData<&'a ()>,
}

impl ToolCx<'_> {
    /// Mints a tool context; the host alone calls this constructor.
    pub(crate) fn new(
        caller: Caller,
        call: CallId,
        session: SessionId,
        turn: Option<TurnId>,
        services: Arc<dyn Services>,
        rt: Arc<dyn ToolCxRuntime>,
        snapshot: CallSnapshot,
    ) -> Self {
        let CallSnapshot {
            generation,
            cutoff,
            env,
        } = snapshot;
        Self {
            caller,
            generation,
            cutoff,
            call,
            session,
            turn,
            services,
            output: OutputSink::new(),
            rt,
            consumer: Consumer::Model,
            env,
            script: None,
            _marker: PhantomData,
        }
    }

    /// Mints a test context over `services` with a fail-closed stub runtime.
    ///
    /// Authorization denies, spawning denies, schemes resolve nothing; the
    /// workspace points at the platform temp dir, which the stub never
    /// writes. Foreign test batteries drive tools through this context.
    #[must_use]
    pub fn for_test(services: Arc<dyn Services>) -> ToolCx<'static> {
        let caller = Caller::new(
            Name::test(),
            Origin::Builtin,
            ServiceSet::EMPTY,
            std::num::NonZeroU32::MIN,
            CallerKind::Tool,
            None,
        );
        let workspace = test_workspace();
        let env = Arc::new(Env {
            vars: std::collections::BTreeMap::new(),
            cwd: workspace.as_path().to_path_buf(),
            sandbox_helper: None,
        });
        ToolCx::new(
            caller,
            CallId::new("test"),
            SessionId::new_v7(),
            None,
            services,
            Arc::new(ForTestRuntime {
                workspace,
                cancel: CancellationToken::new(),
                approve: false,
            }),
            CallSnapshot {
                generation: GenerationId::new(std::num::NonZeroU64::MIN),
                cutoff: None,
                env,
            },
        )
    }

    /// Mints a test context whose authorization always approves.
    ///
    /// The approval grants no argv prefix and no writable roots, so
    /// spawning still denies and no scheme resolves; the ladder itself is
    /// the only difference. Foreign test batteries drive approval-gated
    /// tools through this context without standing up a front end.
    #[must_use]
    pub fn for_test_approved(services: Arc<dyn Services>) -> ToolCx<'static> {
        let caller = Caller::new(
            Name::test(),
            Origin::Builtin,
            ServiceSet::EMPTY,
            std::num::NonZeroU32::MIN,
            CallerKind::Tool,
            None,
        );
        let workspace = test_workspace();
        let env = Arc::new(Env {
            vars: std::collections::BTreeMap::new(),
            cwd: workspace.as_path().to_path_buf(),
            sandbox_helper: None,
        });
        ToolCx::new(
            caller,
            CallId::new("test"),
            SessionId::new_v7(),
            None,
            services,
            Arc::new(ForTestRuntime {
                workspace,
                cancel: CancellationToken::new(),
                approve: true,
            }),
            CallSnapshot {
                generation: GenerationId::new(std::num::NonZeroU64::MIN),
                cutoff: None,
                env,
            },
        )
    }

    /// Runs the approval ladder over `preview` and mints the spawn proof.
    ///
    /// A headless run with no one to ask, a decline, or a timeout returns
    /// the denial; nothing spawns on that path.
    pub fn authorize(&mut self, preview: Preview) -> BoxFuture<'_, Result<Approved, DenyReason>> {
        let cancel = self.rt.cancel();
        self.rt.authorize(&self.call, preview, cancel)
    }

    /// Launches one checked child, consuming the approval proof exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Denied`] when the proof was minted for another
    /// call, when the proof does not cover `argv`, when a path escapes the
    /// approved roots, or when the child cannot start.
    pub fn spawn(
        &mut self,
        argv: &[OsString],
        opts: SpawnOpts,
        approved: Approved,
    ) -> Result<Proc, ToolError> {
        if approved.call() != &self.call {
            return Err(ToolError::Denied(DenyReason::out_of_scope(format!(
                "approval for call {} cannot spawn this call",
                approved.call().as_str()
            ))));
        }
        self.rt.spawn(argv, opts, approved)
    }

    /// Parks a spawned child as a background job and returns its identity.
    #[must_use]
    pub fn detach(self, proc: Proc) -> JobId {
        self.rt.detach(proc)
    }

    /// Borrows the host-minted caller needed for [`Services`] scoping.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// Returns the turn's retained generation snapshot id.
    #[must_use]
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the context-token count captured at turn start, when the
    /// host could supply one.
    #[must_use]
    pub fn cutoff(&self) -> Option<u64> {
        self.cutoff
    }

    /// Borrows the capability-scoped service handle for this call.
    #[must_use]
    pub fn services(&self) -> Arc<dyn Services> {
        Arc::clone(&self.services)
    }

    /// Resolves one `scheme://` URI to its page text.
    #[must_use]
    pub fn resolve(&self, uri: &str) -> BoxFuture<'_, Result<Doc, ToolError>> {
        self.rt.resolve(
            uri,
            SchemeResolveContext {
                caller: &self.caller,
                services: &self.services,
                session: self.session,
            },
        )
    }

    /// Borrows the session workspace this call runs against.
    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        self.rt.workspace()
    }

    /// Borrows the cancellation token for this call.
    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        self.rt.cancel()
    }

    /// Returns the session this call runs against.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the owning turn when the dispatch captured one.
    #[must_use]
    pub fn turn(&self) -> Option<TurnId> {
        self.turn
    }

    /// Borrows the bounded foreground-output buffer for this call.
    pub fn output(&mut self) -> &mut OutputSink {
        &mut self.output
    }

    /// Rebinds the evidence consumer the call's reads are delivered to (R06).
    #[must_use]
    pub fn with_consumer(mut self, consumer: Consumer) -> Self {
        self.consumer = consumer;
        self
    }

    /// Returns the evidence consumer; the root model unless rebound.
    #[must_use]
    pub fn consumer(&self) -> Consumer {
        self.consumer
    }

    /// Attaches the script context of a scripted tool call.
    #[must_use]
    pub fn with_script(mut self, script: ScriptCx) -> Self {
        self.script = Some(script);
        self
    }

    /// Borrows the script context, present only for scripted tool calls.
    #[must_use]
    pub fn script(&self) -> Option<&ScriptCx> {
        self.script.as_ref()
    }

    /// Borrows the host's process-edge snapshot: environment variables and
    /// working directory captured once at host start.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }
}

/// Fail-closed stub backing for [`ToolCx::for_test`].
///
/// Nothing is granted here: authorization has no frontend, spawning is
/// denied, schemes resolve nothing, and detaching returns an untracked
/// identity. Tests drive tools through this context; they never drive the
/// host. `for_test_approved` flips `approve` so the ladder alone passes.
struct ForTestRuntime {
    workspace: Workspace,
    cancel: CancellationToken,
    approve: bool,
}

impl ToolCxRuntime for ForTestRuntime {
    fn authorize(
        &self,
        call: &CallId,
        preview: Preview,
        _cancel: &CancellationToken,
    ) -> BoxFuture<'_, Result<Approved, DenyReason>> {
        let digest = preview.digest;
        let proof = if self.approve {
            Ok(Approved::new(
                call.clone(),
                digest,
                Box::new([]),
                Box::new([]),
                None,
            ))
        } else {
            Err(DenyReason::NoFrontEnd)
        };
        Box::pin(async move { proof })
    }

    fn spawn(
        &self,
        _argv: &[OsString],
        _opts: SpawnOpts,
        _approved: Approved,
    ) -> Result<Proc, ToolError> {
        Err(ToolError::Denied(DenyReason::NoFrontEnd))
    }

    fn detach(&self, _proc: Proc) -> JobId {
        JobId::new_v7()
    }

    fn resolve(
        &self,
        uri: &str,
        _context: SchemeResolveContext<'_>,
    ) -> BoxFuture<'_, Result<Doc, ToolError>> {
        let uri: Box<str> = uri.into();
        Box::pin(async move { Err(ToolError::Scheme(SchemeError::NotFound { uri })) })
    }

    fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

/// Builds the test workspace root from the platform temp dir.
///
/// The path is absolute by construction on every supported platform; the
/// dead arm only satisfies the type system and never runs.
fn test_workspace() -> Workspace {
    let dir = std::env::temp_dir();
    #[cfg(unix)]
    let dir = if dir.is_absolute() {
        dir
    } else {
        PathBuf::from("/").join(dir)
    };
    #[cfg(windows)]
    let dir = if dir.is_absolute() {
        dir
    } else {
        PathBuf::from(r"C:\").join(dir)
    };
    match Workspace::new(dir) {
        Ok(workspace) => workspace,
        Err(_) => loop {
            std::thread::park();
        },
    }
}
