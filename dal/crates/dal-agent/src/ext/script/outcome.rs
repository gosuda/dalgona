//! Operation outcomes and scope scheduling values of the script host seam (R07 E05 E07).

use dal_core::ext::{HookEvent, OpId, OpSet, StateRecord, ToolData};
use dal_core::{CallId, DenyReason, JobId, Name, RawJson, ScopeSpecError};
use std::sync::Arc;

use super::{ScopeId, TaskId};

/// One operation request with canonical arguments (R03).
#[derive(Clone)]
pub struct OpRequest {
    /// The requested operation.
    pub op: OpId,
    /// The canonical bound arguments.
    pub args: RawJson,
    pub(crate) model_runtime: Option<Arc<dyn super::super::ModelCxRuntime>>,
    pub(crate) private_tools: Vec<super::super::PrivateTool>,
}

impl OpRequest {
    /// Builds one request without model-handler context.
    #[must_use]
    pub fn new(op: OpId, args: RawJson) -> Self {
        Self {
            op,
            args,
            model_runtime: None,
            private_tools: Vec::new(),
        }
    }

    /// Carries a model handler's private runtime state to its native op.
    #[must_use]
    pub fn with_model_context(mut self, script: &super::ScriptCx) -> Self {
        self.model_runtime.clone_from(&script.model_runtime);
        self
    }

    /// Carries the private tools bound to a model inference operation.
    #[must_use]
    pub fn with_private_tools(mut self, tools: Vec<super::super::PrivateTool>) -> Self {
        self.private_tools = tools;
        self
    }
}

impl std::fmt::Debug for OpRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpRequest")
            .field("op", &self.op)
            .field("args", &self.args)
            .field("model_context", &self.model_runtime.is_some())
            .field("private_tools", &self.private_tools.len())
            .finish()
    }
}

/// The successful value of one operation (R07).
#[derive(Clone, Debug)]
pub enum OpValue {
    /// A plain JSON value.
    Json(RawJson),
    /// Typed tool data.
    Data(ToolData),
    /// A state record.
    State(StateRecord),
    /// A detached background job; never a zero exit.
    Detached(JobId),
}

/// The effect status recorded for one accepted operation (R07).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectStatus {
    /// The effect completed.
    Completed,
    /// The operation completed without a change.
    NoChange,
    /// The operation failed.
    Failed,
    /// The operation was cancelled.
    Cancelled,
    /// The effect's outcome is unknown.
    Unresolved,
}

/// The ledger record of one accepted operation (R07).
#[derive(Clone, Debug)]
pub struct OpRecord {
    /// The core call identity.
    pub call: CallId,
    /// The operation.
    pub op: OpId,
    /// The recorded effect status.
    pub status: EffectStatus,
}

/// The code of one expected, recoverable failure (R07 R08 E05 E06).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureCode {
    /// A generic service failure.
    Failed,
    /// A process exited nonzero.
    ExitNonZero,
    /// A known operation is unavailable.
    Unavailable,
    /// A state revision conflict.
    Conflict,
    /// Dependency admission refused a start.
    Busy,
    /// The owner cancelled the task.
    Cancelled,
    /// An observation reference is not available.
    ObservationUnavailable,
    /// A handle belongs to another invocation.
    InvocationMismatch,
    /// A recorded effect cannot be resolved.
    Indeterminate,
    /// A plugin domain failure code.
    Domain(Box<str>),
}

impl FailureCode {
    /// Returns the wire code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Failed => "failed",
            Self::ExitNonZero => "exit_nonzero",
            Self::Unavailable => "unavailable",
            Self::Conflict => "conflict",
            Self::Busy => "busy",
            Self::Cancelled => "cancelled",
            Self::ObservationUnavailable => "observation_unavailable",
            Self::InvocationMismatch => "invocation_mismatch",
            Self::Indeterminate => "indeterminate",
            Self::Domain(code) => code,
        }
    }
}

/// An expected operation failure a script may recover from (R07).
#[derive(Clone, Debug, thiserror::Error)]
#[error("{}: {message}", code.as_str())]
pub struct OpFailure {
    /// The failure code.
    pub code: FailureCode,
    /// The failure message.
    pub message: Box<str>,
    /// Optional structured details.
    pub details: Option<RawJson>,
}

/// A terminal host outcome no script can catch (R04 R07 R10 E01 E05).
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HostTerminal {
    /// The operation failed the authority check.
    #[error("denied: {reason:?}")]
    Denied {
        /// Why it was denied.
        reason: DenyReason,
    },
    /// A `uses` entry lies outside the environment.
    #[error("scope_exceeded: {op}")]
    ScopeExceeded {
        /// The first operation outside `A`.
        op: OpId,
    },
    /// A composite export's primitives are missing from `U`.
    #[error("incomplete_scope")]
    IncompleteScope {
        /// The missing primitives.
        missing: OpSet,
    },
    /// A plugin handler tried to enter another script export.
    #[error("nested_script_export")]
    NestedScriptExport,
    /// An eval tried to start another eval.
    #[error("nested_eval")]
    NestedEval,
    /// The root was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A hard host limit was reached.
    #[error("limit_exceeded: {what}")]
    LimitExceeded {
        /// The exceeded limit.
        what: &'static str,
    },
    /// A scope was rejected because its specification is invalid.
    #[error("invalid_scope: {0}")]
    InvalidScope(ScopeSpecError),
    /// A grant was revoked before acceptance.
    #[error("revoked")]
    Revoked,
}

/// The outcome of one operation (R07).
#[derive(Clone, Debug)]
pub enum OpOutcome {
    /// The operation succeeded.
    Ok {
        /// The value.
        value: OpValue,
        /// The ledger record.
        record: OpRecord,
    },
    /// The operation failed recoverably.
    Failed {
        /// The failure.
        failure: OpFailure,
        /// The ledger record.
        record: OpRecord,
    },
    /// The invocation ends; the gate is closed.
    Terminal(HostTerminal),
}

/// The result of submitting a scope task (E05).
#[derive(Clone, Debug)]
pub enum Submit {
    /// The task is queued.
    Queued(TaskId),
    /// The task settled before launch, such as busy admission.
    Settled(TaskId),
    /// The scope refused the submission under its budget.
    Refused(ScopeSpecError),
    /// The submission ends the invocation.
    Terminal(HostTerminal),
}

/// What a collection awaits (E05).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Collect {
    /// One task.
    Task(TaskId),
    /// Every task of a scope, sealing it.
    Seal(ScopeId),
}

/// What a cancellation targets (E05).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelTarget {
    /// One task.
    Task(TaskId),
    /// A whole scope.
    Scope(ScopeId),
}

/// One recorded observer hook failure (P05).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObserverError {
    /// The extension whose hook failed.
    pub ext: Name,
    /// The observed event.
    pub event: HookEvent,
    /// The failure message.
    pub message: Box<str>,
}

/// The cleanup report of a finished invocation (E07).
#[derive(Clone, Debug, Default)]
pub struct Cleanup {
    /// Whether every owned operation settled.
    pub complete: bool,
    /// The calls still unsettled at the cleanup bound.
    pub outstanding: Box<[CallId]>,
    /// The observer failures recorded for the root.
    pub observer_errors: Box<[ObserverError]>,
}
