//! A deterministic recording [`ScriptHost`] double plus reply and service
//! helpers for the runtime tests.

#![expect(
    clippy::expect_used,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ServiceError;
use dal_agent::ext::generation::catalog::Catalog;
use dal_agent::ext::script::{
    CancelTarget, Cleanup, Collect, EffectStatus, Entry, EvalEnvironment, FailureCode,
    HostTerminal, Invocation, InvocationId, OpFailure, OpOutcome, OpRecord, OpRequest, OpValue,
    Parent, ScopeId, ScriptCx, ScriptHost, Submit, TaskId,
};
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::tool::{RawValue, ToolCx, ToolOutcome};
use dal_agent::ext::{Caller, Extension, Services};
use dal_core::ext::{NativeOp, OpId, OpSet, Phase};
use dal_core::{
    AgentsOp, AgentsReply, Answer, CallId, EntryId, FetchRequest, FetchResponse, GenerationId,
    Inference, JobsOp, JobsReply, McpRequest, McpResponse, ModelRequest, Notice, Question, RawJson,
    RunOutput, RunRequest, ScopeSpec, SidecarOp, StateError, StateOp, StateRecord, TurnOp,
    TurnOpReply,
};

/// One host-facets interaction observed by `RecordingHost`, in call order.
#[derive(Clone, Debug)]
pub enum HostRecord {
    /// A cell was admitted through `begin`.
    Begin {
        /// Parent invocation, or `None` for a root cell.
        parent: Option<InvocationId>,
        /// The admitted entry declaration.
        entry: Entry,
        /// The minted invocation.
        invocation: InvocationId,
        /// The effect ceiling resolved for the entry.
        ceiling: OpSet,
        /// The execution phase of the entry.
        phase: Phase,
    },
    /// A host facets call was received.
    Call {
        /// The calling invocation.
        invocation: InvocationId,
        /// The caller name from the invocation environment.
        caller: Box<str>,
        /// The requested operation.
        op: OpId,
        /// The raw call arguments.
        args: RawJson,
    },
    /// A scope was opened.
    OpenScope {
        /// The opening invocation.
        invocation: InvocationId,
        /// The requested scope specification.
        spec: ScopeSpec,
        /// The minted scope id.
        scope: ScopeId,
    },
    /// A background operation was submitted.
    Submit {
        /// The submitting invocation.
        invocation: InvocationId,
        /// The scope receiving the submit.
        scope: ScopeId,
        /// The submitted request.
        request: OpRequest,
    },
    /// A collect was requested.
    Collect {
        /// The collecting invocation.
        invocation: InvocationId,
        /// What is being collected.
        which: Collect,
    },
    /// A cancel was delivered.
    Cancel {
        /// The cancelling invocation.
        invocation: InvocationId,
        /// What is being cancelled.
        target: CancelTarget,
    },
    /// An invocation finished.
    Finish {
        /// The finished invocation.
        invocation: InvocationId,
    },
}

/// A host misbehavior injected into the async half of a host call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    /// `call` panics inside its future.
    Call,
    /// `collect` panics inside its future.
    Collect,
}

/// A deterministic host for exercising Starlark adapters at `ScriptHost`.
pub struct RecordingHost {
    fault: Option<Fault>,
    environment: Arc<EvalEnvironment>,
    catalog: Catalog,
    replies: Mutex<HashMap<OpId, OpOutcome>>,
    records: Mutex<Vec<HostRecord>>,
    next_scope_id: AtomicU64,
    next_task_id: AtomicU64,
    budget: Duration,
}

impl RecordingHost {
    /// Builds a host with the default one-minute budget and scripted replies.
    #[must_use]
    pub fn new(
        extensions: &[Extension],
        allowed: OpSet,
        replies: impl IntoIterator<Item = (OpId, OpOutcome)>,
    ) -> Arc<Self> {
        Self::with_budget(extensions, allowed, replies, Duration::from_secs(60))
    }

    /// Builds a host with scripted replies and an explicit budget.
    #[must_use]
    pub fn with_budget(
        extensions: &[Extension],
        allowed: OpSet,
        replies: impl IntoIterator<Item = (OpId, OpOutcome)>,
        budget: Duration,
    ) -> Arc<Self> {
        Self::build(extensions, allowed, replies, budget, None)
    }

    /// A host whose `fault` fires the first time the faulted call runs.
    #[must_use]
    pub fn faulty(extensions: &[Extension], fault: Fault) -> Arc<Self> {
        Self::build(
            extensions,
            OpSet::EMPTY,
            [],
            Duration::from_secs(60),
            Some(fault),
        )
    }

    fn build(
        extensions: &[Extension],
        allowed: OpSet,
        replies: impl IntoIterator<Item = (OpId, OpOutcome)>,
        budget: Duration,
        fault: Option<Fault>,
    ) -> Arc<Self> {
        let generation = GenerationId::new(NonZeroU64::MIN);
        let environment = EvalEnvironment::capture(generation, allowed, None, [0; 32])
            .expect("test environment is below the capture limit");
        Arc::new(Self {
            fault,
            environment: Arc::new(environment),
            catalog: Catalog::for_test(extensions),
            replies: Mutex::new(replies.into_iter().collect()),
            records: Mutex::new(Vec::new()),
            next_scope_id: AtomicU64::new(1),
            next_task_id: AtomicU64::new(1),
            budget,
        })
    }

    /// Returns a script context that runs cells against this host.
    #[must_use]
    pub fn script_cx(self: &Arc<Self>) -> ScriptCx {
        ScriptCx::new(
            Arc::clone(self) as Arc<dyn ScriptHost>,
            Arc::clone(&self.environment),
            None,
        )
    }

    /// Returns every host-facets record observed so far, oldest first.
    ///
    /// # Panics
    ///
    /// Panics if the record log lock is poisoned.
    #[must_use]
    pub fn records(&self) -> Vec<HostRecord> {
        self.records.lock().expect("host records lock").clone()
    }

    fn record(&self, item: HostRecord) {
        self.records.lock().expect("host records lock").push(item);
    }
}

impl ScriptHost for RecordingHost {
    fn begin(&self, parent: Parent<'_>, entry: Entry) -> Result<Arc<Invocation>, HostTerminal> {
        let parent_id = match parent {
            Parent::Root => None,
            Parent::Of(invocation) => Some(invocation.id()),
        };
        let (ceiling, phase) = match &entry {
            Entry::Eval { uses } => (self.environment.resolve(uses.as_ref())?, Phase::Eval),
            Entry::Export { id, phase } => {
                let declaration = self.catalog.export(id).ok_or(HostTerminal::Denied {
                    reason: dal_agent::DenyReason::Unavailable {
                        what: format!("unknown script export {id:?}").into(),
                    },
                })?;
                (declaration.uses.clone(), *phase)
            }
        };
        let invocation = Invocation::for_test(ceiling.clone(), phase, self.budget)?;
        self.record(HostRecord::Begin {
            parent: parent_id,
            entry,
            invocation: invocation.id(),
            ceiling,
            phase,
        });
        Ok(invocation)
    }

    fn call(
        &self,
        invocation: &Arc<Invocation>,
        request: OpRequest,
    ) -> dal_agent::ext::BoxFuture<'static, OpOutcome> {
        self.record(HostRecord::Call {
            invocation: invocation.id(),
            caller: invocation.caller().ext().as_str().into(),
            op: request.op.clone(),
            args: request.args.clone(),
        });
        let reply = self
            .replies
            .lock()
            .expect("host reply lock")
            .get(&request.op)
            .cloned()
            .unwrap_or_else(|| {
                failed(
                    request.op.clone(),
                    FailureCode::Unavailable,
                    "no scripted reply",
                )
            });
        let explode = self.fault == Some(Fault::Call);
        Box::pin(async move {
            assert!(!explode, "injected host call fault");
            reply
        })
    }

    fn open_scope(
        &self,
        invocation: &Arc<Invocation>,
        spec: ScopeSpec,
    ) -> Result<ScopeId, HostTerminal> {
        let scope = ScopeId::new(allocate_id(&self.next_scope_id, "scope ids")?);
        self.record(HostRecord::OpenScope {
            invocation: invocation.id(),
            spec,
            scope,
        });
        Ok(scope)
    }

    fn submit(&self, invocation: &Arc<Invocation>, scope: ScopeId, request: OpRequest) -> Submit {
        self.record(HostRecord::Submit {
            invocation: invocation.id(),
            scope,
            request,
        });
        match allocate_id(&self.next_task_id, "task ids") {
            Ok(id) => Submit::Settled(TaskId::new(id)),
            Err(terminal) => Submit::Terminal(terminal),
        }
    }

    fn collect(
        &self,
        invocation: &Arc<Invocation>,
        which: Collect,
    ) -> dal_agent::ext::BoxFuture<'static, Result<dal_agent::ext::script::Collected, HostTerminal>>
    {
        self.record(HostRecord::Collect {
            invocation: invocation.id(),
            which,
        });
        let explode = self.fault == Some(Fault::Collect);
        Box::pin(async move {
            assert!(!explode, "injected host collect fault");
            Ok(Box::default())
        })
    }

    fn cancel(&self, invocation: &Arc<Invocation>, target: CancelTarget) {
        self.record(HostRecord::Cancel {
            invocation: invocation.id(),
            target,
        });
    }

    fn adopt(
        &self,
        _invocation: &Arc<Invocation>,
        _reference: &str,
    ) -> Result<dal_core::ReadView, OpFailure> {
        Err(OpFailure {
            code: FailureCode::ObservationUnavailable,
            message: "test host has no evidence".into(),
            details: None,
        })
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn finish(&self, invocation: &Arc<Invocation>) -> dal_agent::ext::BoxFuture<'static, Cleanup> {
        self.record(HostRecord::Finish {
            invocation: invocation.id(),
        });
        Box::pin(async {
            Cleanup {
                complete: true,
                outstanding: Box::default(),
                observer_errors: Box::default(),
            }
        })
    }
}

fn allocate_id(counter: &AtomicU64, what: &'static str) -> Result<NonZeroU64, HostTerminal> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let Some(next) = current.checked_add(1) else {
            return Err(HostTerminal::LimitExceeded { what });
        };
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(previous) => {
                return NonZeroU64::new(previous).ok_or(HostTerminal::LimitExceeded { what });
            }
            Err(actual) => current = actual,
        }
    }
}

/// Builds a failed operation reply with the given code and message.
#[must_use]
pub fn failed(op: OpId, code: FailureCode, message: &str) -> OpOutcome {
    OpOutcome::Failed {
        failure: OpFailure {
            code,
            message: message.into(),
            details: None,
        },
        record: OpRecord {
            call: CallId::new("script-host-test"),
            op,
            status: EffectStatus::Failed,
        },
    }
}

/// A successful operation reply carrying raw JSON text.
///
/// # Panics
///
/// Panics if `json` is not well-formed JSON.
#[must_use]
pub fn completed_json(op: OpId, json: &str) -> OpOutcome {
    OpOutcome::Ok {
        value: OpValue::Json(RawJson::parse(json).expect("reply JSON is well formed")),
        record: OpRecord {
            call: CallId::new("script-host-test"),
            op,
            status: EffectStatus::Completed,
        },
    }
}

/// Wraps a native operation as an operation id.
#[must_use]
pub fn native_op(op: NativeOp) -> OpId {
    OpId::Native(op)
}

/// Returns a services handle that refuses every effect.
#[must_use]
pub fn test_services() -> Arc<dyn Services> {
    Arc::new(NoServices)
}

fn unused<T>() -> ServiceFuture<'static, T> {
    Box::pin(async {
        Err(ServiceError::failed(
            None,
            "service is not configured in this test",
        ))
    })
}

struct NoServices;

impl Services for NoServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unused()
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unused()
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unused()
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unused()
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unused()
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unused()
    }
    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unused()
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unused()
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(
            std::sync::Arc<dyn dal_agent::ext::Tool>,
            dal_core::Visibility,
        )>,
    ) -> ServiceFuture<'_, ()> {
        unused()
    }
    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        unused()
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unused()
    }
    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unused()
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unused()
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        unused()
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unused()
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unused()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unused()
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unused()
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unused()
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unused()
    }
    fn infer_stream(
        &self,
        _who: &Caller,
        _req: ModelRequest,
    ) -> ServiceFuture<'_, dal_provider::EventStream> {
        unused()
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unused()
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {}
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unused()
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        unused()
    }
}

/// Builds a tool context over `test_services` and the given script context.
#[must_use]
pub fn tool_cx(script: ScriptCx) -> ToolCx<'static> {
    ToolCx::for_test(test_services()).with_script(script)
}

/// Builds a tool context whose authorization approves every request.
#[must_use]
pub fn tool_cx_approved(script: ScriptCx) -> ToolCx<'static> {
    ToolCx::for_test_approved(test_services()).with_script(script)
}

/// A pure eval request (`uses = []`): `ToolCx::for_test` authorization is
/// fail-closed, so a cell that asks for effects would be denied before it
/// runs. `RecordingHost` does not gate calls on the ceiling, so a pure cell
/// still reaches the scripted replies.
///
/// # Panics
///
/// Panics if the code cannot be encoded as a JSON string.
#[must_use]
pub fn eval_args(code: &str) -> String {
    format!(
        "{{\"code\":{},\"uses\":[]}}",
        sonic_rs::to_string(code).expect("string JSON")
    )
}
