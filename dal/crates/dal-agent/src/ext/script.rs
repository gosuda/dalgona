//! The host seam every script invocation runs through (R02 R03 R04 R06 R07 E01 E05 E06 E07).
//!
//! An [`Invocation`] is minted only by the host through [`ScriptHost::begin`];
//! scripts never set its identity, ceiling, phase, consumer, or deadline.
//! Every operation a script requests goes through [`ScriptHost::call`] or a
//! scope, and returns an [`OpOutcome`] whose category decides whether the
//! script may recover.

use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use dal_core::ext::{Consumer, ExportId, OpId, OpSet, Phase, ReadView};
use dal_core::{GenerationId, ScopeSpec, SessionId};
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::generation::catalog::Catalog;
use super::{BoxFuture, Caller};

mod outcome;
#[cfg(test)]
mod tests;

pub use outcome::{
    CancelTarget, Cleanup, Collect, EffectStatus, FailureCode, HostTerminal, ObserverError,
    OpFailure, OpOutcome, OpRecord, OpRequest, OpValue, Submit,
};

/// The largest number of native operations one invocation tree may issue (R10).
pub const MAX_ISSUED: u32 = 512;

/// The deepest nested host invocation (R10).
pub const MAX_DEPTH: u8 = 8;

/// The largest effective root ceiling (R10 E01).
pub const MAX_CEILING: usize = 64;

macro_rules! handle_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Wraps a host-assigned number.
            #[must_use]
            pub const fn new(value: NonZeroU64) -> Self {
                Self(value)
            }

            /// Returns the host-assigned number.
            #[must_use]
            pub const fn get(self) -> NonZeroU64 {
                self.0
            }
        }
    };
}

handle_id!(
    InvocationId,
    "The identity of one host-minted invocation (R02)."
);
handle_id!(ScopeId, "The identity of one invocation-owned scope (E05).");
handle_id!(TaskId, "The identity of one scheduled scope task (E05).");

/// One reserved interpreter worker; released only when dropped (R09).
///
/// Only host admission mints the permit through its crate-private constructor. An [`Invocation`] holds its permit
/// until the worker thread drops the last reference, so a quarantined
/// worker keeps counting against the cap.
#[derive(Debug)]
pub struct WorkerPermit {
    _permit: OwnedSemaphorePermit,
}

impl WorkerPermit {
    /// Mints the permit host admission reserved (R09).
    #[must_use]
    pub(crate) fn new(permit: OwnedSemaphorePermit) -> Self {
        Self { _permit: permit }
    }
}

/// The per-root switch that terminal outcomes close (R07).
///
/// Once closed, no further effect of the invocation tree may start.
#[derive(Debug)]
pub struct EffectGate(AtomicBool);

impl EffectGate {
    /// Builds an open gate.
    #[must_use]
    pub fn open() -> Self {
        Self(AtomicBool::new(true))
    }

    /// Reports whether effects may still start.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Closes the gate; closing only removes authority, so any holder may.
    pub fn close(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The host-owned parts of one invocation, assembled by the host before minting.
pub(crate) struct InvocationParts {
    pub(crate) parent: Option<Arc<Invocation>>,
    pub(crate) session: SessionId,
    pub(crate) generation: GenerationId,
    pub(crate) caller: Caller,
    pub(crate) export: Option<ExportId>,
    pub(crate) phase: Phase,
    pub(crate) consumer: Option<Consumer>,
    pub(crate) deadline: Instant,
    pub(crate) ceiling: OpSet,
    pub(crate) declared: OpSet,
    pub(crate) cutoff: Option<u64>,
    pub(crate) cancel: CancellationToken,
    pub(crate) permit: Option<WorkerPermit>,
}

/// A host-minted invocation identity (R02 R04).
///
/// There are no setters. The ceiling `U`, declaration `D`, phase, consumer,
/// deadline, and cancellation are fixed at minting; a child inherits the
/// root's effect gate and issued-operation counter.
#[derive(Debug)]
pub struct Invocation {
    id: InvocationId,
    root: InvocationId,
    parent: Option<Arc<Invocation>>,
    depth: u8,
    session: SessionId,
    generation: GenerationId,
    caller: Caller,
    export: Option<ExportId>,
    phase: Phase,
    consumer: Consumer,
    deadline: Instant,
    ceiling: OpSet,
    declared: OpSet,
    cutoff: Option<u64>,
    cancel: CancellationToken,
    gate: Arc<EffectGate>,
    issued: Arc<AtomicU32>,
    permit: Option<WorkerPermit>,
}

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(1);

fn next_invocation() -> Result<InvocationId, HostTerminal> {
    let raw = NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed);
    NonZeroU64::new(raw)
        .map(InvocationId)
        .ok_or(HostTerminal::LimitExceeded {
            what: "invocation ids",
        })
}

impl Invocation {
    /// Mints an invocation; a `parent` in `parts` makes it a nested child.
    ///
    /// A child shares its root's gate and issued counter, is at most
    /// [`MAX_DEPTH`] deep, and can never widen its parent: its ceiling is
    /// intersected with the parent's, its deadline is the earlier one, and
    /// its cancellation is a child of the parent's token (R04 R10).
    pub(crate) fn mint(parts: InvocationParts) -> Result<Arc<Self>, HostTerminal> {
        let id = next_invocation()?;
        let InvocationParts {
            parent,
            mut ceiling,
            mut deadline,
            mut cancel,
            ..
        } = parts;
        let (root, depth, gate, issued) = match &parent {
            None => (
                id,
                0,
                Arc::new(EffectGate::open()),
                Arc::new(AtomicU32::new(0)),
            ),
            Some(parent) => {
                let depth = parent.depth.saturating_add(1);
                if depth > MAX_DEPTH {
                    return Err(HostTerminal::LimitExceeded { what: "depth" });
                }
                ceiling = ceiling.intersect(&parent.ceiling);
                deadline = deadline.min(parent.deadline);
                cancel = parent.cancel.child_token();
                (
                    parent.root,
                    depth,
                    Arc::clone(&parent.gate),
                    Arc::clone(&parent.issued),
                )
            }
        };
        let consumer = parts.consumer.unwrap_or(Consumer::Invocation(id.get()));
        Ok(Arc::new(Self {
            id,
            root,
            parent,
            depth,
            session: parts.session,
            generation: parts.generation,
            caller: parts.caller,
            export: parts.export,
            phase: parts.phase,
            consumer,
            deadline,
            ceiling,
            declared: parts.declared,
            cutoff: parts.cutoff,
            cancel,
            gate,
            issued,
            permit: parts.permit,
        }))
    }

    /// Mints a root invocation for tests with no worker permit whose
    /// deadline falls `budget` after now.
    ///
    /// # Errors
    /// Returns [`HostTerminal::LimitExceeded`] once invocation ids run out.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        ceiling: OpSet,
        phase: Phase,
        budget: std::time::Duration,
    ) -> Result<Arc<Self>, HostTerminal> {
        let parts = InvocationParts {
            parent: None,
            session: SessionId::new_v7(),
            generation: GenerationId::new(NonZeroU64::MIN),
            caller: Caller::new(
                dal_core::Name::test(),
                dal_core::Origin::Builtin,
                dal_core::ServiceSet::EMPTY,
                super::CallerKind::Handler,
                None,
            ),
            export: None,
            phase,
            consumer: None,
            deadline: Instant::now() + budget,
            declared: ceiling.clone(),
            ceiling,
            cutoff: None,
            cancel: CancellationToken::new(),
            permit: None,
        };
        Self::mint(parts)
    }

    /// Charges one issued native operation to the invocation tree (R10).
    ///
    /// # Errors
    /// Returns [`HostTerminal::LimitExceeded`] once the tree has issued
    /// [`MAX_ISSUED`] operations.
    pub fn issue(&self) -> Result<u32, HostTerminal> {
        self.issued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |issued| {
                (issued < MAX_ISSUED).then_some(issued + 1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| HostTerminal::LimitExceeded {
                what: "issued operations",
            })
    }

    /// Returns the invocation identity.
    #[must_use]
    pub fn id(&self) -> InvocationId {
        self.id
    }

    /// Returns the root invocation identity.
    #[must_use]
    pub fn root(&self) -> InvocationId {
        self.root
    }

    /// Returns the parent invocation, absent for a root.
    #[must_use]
    pub fn parent(&self) -> Option<InvocationId> {
        self.parent.as_ref().map(|parent| parent.id)
    }

    /// Returns the nesting depth; a root is zero.
    #[must_use]
    pub fn depth(&self) -> u8 {
        self.depth
    }

    /// Returns the owning session.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the captured catalog generation.
    #[must_use]
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Borrows the host-minted caller.
    #[must_use]
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// Returns the export entered, absent for eval.
    #[must_use]
    pub fn export(&self) -> Option<&ExportId> {
        self.export.as_ref()
    }

    /// Borrows the root ceiling `U`.
    #[must_use]
    pub fn ceiling(&self) -> &OpSet {
        &self.ceiling
    }

    /// Borrows the current declaration `D`.
    #[must_use]
    pub fn declared(&self) -> &OpSet {
        &self.declared
    }

    /// Returns the caller phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Returns the evidence consumer the invocation reads as.
    #[must_use]
    pub fn consumer(&self) -> Consumer {
        self.consumer
    }

    /// Returns the wall deadline.
    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns the frozen parent observation cutoff.
    #[must_use]
    pub fn cutoff(&self) -> Option<u64> {
        self.cutoff
    }

    /// Borrows the cancellation token.
    #[must_use]
    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Borrows the tree's effect gate.
    #[must_use]
    pub fn gate(&self) -> &EffectGate {
        &self.gate
    }

    /// Reports whether the invocation owns a worker permit.
    #[must_use]
    pub fn holds_permit(&self) -> bool {
        self.permit.is_some()
    }

    /// Reports whether `op` passes the static `U ∩ D ∩ P` part of R04.
    ///
    /// Live grants `G` and concrete approval are checked by the host after
    /// this and are not part of the invocation.
    #[must_use]
    pub fn allows(&self, op: &OpId) -> bool {
        self.ceiling.contains(op) && self.declared.contains(op) && self.phase.permits(op)
    }
}

/// The host-captured eval environment of one decision request (E01).
///
/// It is an out-of-band descriptor, never model-supplied. A missing
/// environment is the empty set `A`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalEnvironment {
    generation: GenerationId,
    allowed: OpSet,
    cutoff: Option<u64>,
    fingerprint: [u8; 32],
}

impl EvalEnvironment {
    /// Captures an environment, rejecting an oversized `A` (R10 E01).
    ///
    /// # Errors
    /// Returns [`HostTerminal::LimitExceeded`] when `A` holds more than
    /// [`MAX_CEILING`] operations.
    pub fn capture(
        generation: GenerationId,
        allowed: OpSet,
        cutoff: Option<u64>,
        fingerprint: [u8; 32],
    ) -> Result<Self, HostTerminal> {
        if allowed.len() > MAX_CEILING {
            return Err(HostTerminal::LimitExceeded {
                what: "environment operations",
            });
        }
        Ok(Self {
            generation,
            allowed,
            cutoff,
            fingerprint,
        })
    }

    /// Resolves a cell ceiling `U` from its optional `uses` (E01).
    ///
    /// Omitted `uses` resolves to `A`; an explicit list must be a subset of
    /// `A`; an empty list is pure execution.
    ///
    /// # Errors
    /// Returns [`HostTerminal::ScopeExceeded`] naming the first operation
    /// outside `A`.
    pub fn resolve(&self, uses: Option<&OpSet>) -> Result<OpSet, HostTerminal> {
        let Some(uses) = uses else {
            return Ok(self.allowed.clone());
        };
        match uses.iter().find(|op| !self.allowed.contains(op)) {
            Some(op) => Err(HostTerminal::ScopeExceeded { op }),
            None => Ok(uses.clone()),
        }
    }

    /// Returns the captured generation.
    #[must_use]
    pub fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Borrows the allowed set `A`.
    #[must_use]
    pub fn allowed(&self) -> &OpSet {
        &self.allowed
    }

    /// Returns the frozen parent observation cutoff.
    #[must_use]
    pub fn cutoff(&self) -> Option<u64> {
        self.cutoff
    }

    /// Returns the captured policy fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
}

/// The script context a host attaches to a tool, command, or hook call.
#[derive(Clone)]
pub struct ScriptCx {
    /// The host seam.
    pub host: Arc<dyn ScriptHost>,
    /// The captured eval environment.
    pub env: Arc<EvalEnvironment>,
    /// The enclosing invocation for nested entries, absent at a root.
    pub parent: Option<Arc<Invocation>>,
    pub(crate) model_runtime: Option<Arc<dyn super::ModelCxRuntime>>,
}

impl ScriptCx {
    /// Creates a script context without model-handler runtime state.
    #[must_use]
    pub fn new(
        host: Arc<dyn ScriptHost>,
        env: Arc<EvalEnvironment>,
        parent: Option<Arc<Invocation>>,
    ) -> Self {
        Self {
            host,
            env,
            parent,
            model_runtime: None,
        }
    }
}

/// The parent of an invocation being begun.
#[derive(Clone, Copy)]
pub enum Parent<'a> {
    /// A new root.
    Root,
    /// A child of an existing invocation.
    Of(&'a Arc<Invocation>),
}

/// The entry an invocation is begun for (R04 E01).
#[derive(Clone, Debug)]
pub enum Entry {
    /// An eval cell with its optional `uses`.
    Eval {
        /// The cell's explicit `uses`, if any.
        uses: Option<OpSet>,
    },
    /// An exported entry in a phase.
    Export {
        /// The entered export.
        id: ExportId,
        /// The phase the export runs in.
        phase: Phase,
    },
}

/// Settled task outcomes of one collection, in submission order (E05).
pub type Collected = Box<[(TaskId, OpOutcome)]>;

/// The host side of the script bridge (R02 R03 R04 R07 E05 E06 E07).
pub trait ScriptHost: Send + Sync + 'static {
    /// Mints an invocation; the only minting door (R02 R04).
    ///
    /// # Errors
    /// Returns a [`HostTerminal`] when the entry is not admitted.
    fn begin(&self, parent: Parent<'_>, entry: Entry) -> Result<Arc<Invocation>, HostTerminal>;

    /// Runs one synchronous operation (R03 R04 R07).
    fn call(&self, inv: &Arc<Invocation>, req: OpRequest) -> BoxFuture<'static, OpOutcome>;

    /// Opens a scope with its concurrency, error policy, and budget bounds (E05).
    ///
    /// # Errors
    /// Returns a [`HostTerminal`] for an invalid spec or when the live-scope cap is reached.
    fn open_scope(&self, inv: &Arc<Invocation>, spec: ScopeSpec) -> Result<ScopeId, HostTerminal>;

    /// Submits one task to a scope (E05).
    fn submit(&self, inv: &Arc<Invocation>, scope: ScopeId, req: OpRequest) -> Submit;

    /// Awaits one task or seals a scope, returning outcomes in submission order (E05).
    fn collect(
        &self,
        inv: &Arc<Invocation>,
        which: Collect,
    ) -> BoxFuture<'static, Result<Collected, HostTerminal>>;

    /// Requests cancellation; idempotent (E05).
    fn cancel(&self, inv: &Arc<Invocation>, target: CancelTarget);

    /// Adopts an observation reference eligible to the parent consumer (E06).
    ///
    /// # Errors
    /// Returns an [`OpFailure`] with [`FailureCode::ObservationUnavailable`]
    /// for any ineligible reference.
    fn adopt(&self, inv: &Arc<Invocation>, reference: &str) -> Result<ReadView, OpFailure>;

    /// Borrows the captured catalog (R03).
    fn catalog(&self) -> &Catalog;

    /// Cancels owned scopes and reports cleanup (E07).
    fn finish(&self, inv: &Arc<Invocation>) -> BoxFuture<'static, Cleanup>;

    /// Records one observer-hook failure against the invocation's root (R07 P05).
    ///
    /// Observer failures never undo or relabel the triggering operation;
    /// the host reports them through [`Cleanup::observer_errors`].
    fn observe_failed(&self, inv: &Arc<Invocation>, error: ObserverError) {
        let _ = (inv, error);
    }
}

/// An observation reference is not eligible for adoption (E06).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AdoptError {
    /// The reference is expired, foreign, or not delivered to the parent.
    #[error(
        "observation_unavailable: the referenced read is not available to this invocation; read the file again"
    )]
    ObservationUnavailable,
}

/// The evidence owner the host delivers views through (R06 E06).
///
/// A generation installs at most one owner.
pub trait Evidence: Send + Sync + 'static {
    /// Records that the complete rows of `view` reached `to` at sequence `at`.
    fn delivered(&self, view: &ReadView, to: Consumer, at: u64);

    /// Adopts a reference eligible to `parent` at `parent_cutoff` for `child`.
    ///
    /// # Errors
    /// Returns [`AdoptError`] for any ineligible reference.
    fn adopt(
        &self,
        session: SessionId,
        reference: &str,
        parent: Consumer,
        parent_cutoff: u64,
        child: Consumer,
    ) -> Result<ReadView, AdoptError>;
}
