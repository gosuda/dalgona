//! A deterministic `ScriptHost` recording every adapter call for assertions.

#![expect(clippy::expect_used, reason = "SC test")]

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ServiceError;
use dal_agent::ext::generation::catalog::Catalog;
use dal_agent::ext::script::{
    CancelTarget, Cleanup, Collect, EffectStatus, Entry, EvalEnvironment, FailureCode,
    HostTerminal, Invocation, InvocationId, OpFailure, OpOutcome, OpRecord, OpRequest, Parent,
    ScopeId, ScriptCx, ScriptHost, Submit, TaskId,
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

/// Recorded host events; fields are kept for failure diagnosis even when a
/// given run only matches on the variant.
#[derive(Clone, Debug)]
#[expect(
    dead_code,
    reason = "recorded for diagnosis; not every field is asserted"
)]
pub(crate) enum HostRecord {
    Begin {
        parent: Option<InvocationId>,
        entry: Entry,
        invocation: InvocationId,
        ceiling: OpSet,
        phase: Phase,
    },
    Call {
        invocation: InvocationId,
        caller: Box<str>,
        op: OpId,
        args: RawJson,
    },
    OpenScope {
        invocation: InvocationId,
        spec: ScopeSpec,
        scope: ScopeId,
    },
    Submit {
        invocation: InvocationId,
        scope: ScopeId,
        request: OpRequest,
    },
    Collect {
        invocation: InvocationId,
        which: Collect,
    },
    Cancel {
        invocation: InvocationId,
        target: CancelTarget,
    },
    Finish {
        invocation: InvocationId,
    },
}

/// A deterministic host for exercising Starlark adapters at `ScriptHost`.
pub(crate) struct RecordingHost {
    environment: Arc<EvalEnvironment>,
    catalog: Catalog,
    replies: Mutex<HashMap<OpId, OpOutcome>>,
    records: Mutex<Vec<HostRecord>>,
    next_scope_id: AtomicU64,
    next_task_id: AtomicU64,
    budget: Duration,
}

impl RecordingHost {
    pub(crate) fn new(
        extensions: &[Extension],
        allowed: OpSet,
        replies: impl IntoIterator<Item = (OpId, OpOutcome)>,
    ) -> Arc<Self> {
        Self::with_budget(extensions, allowed, replies, Duration::from_secs(60))
    }

    pub(crate) fn with_budget(
        extensions: &[Extension],
        allowed: OpSet,
        replies: impl IntoIterator<Item = (OpId, OpOutcome)>,
        budget: Duration,
    ) -> Arc<Self> {
        let generation = GenerationId::new(NonZeroU64::MIN);
        let environment = EvalEnvironment::capture(generation, allowed, None, [0; 32])
            .expect("test environment is below the capture limit");
        Arc::new(Self {
            environment: Arc::new(environment),
            catalog: Catalog::for_test(extensions),
            replies: Mutex::new(replies.into_iter().collect()),
            records: Mutex::new(Vec::new()),
            next_scope_id: AtomicU64::new(1),
            next_task_id: AtomicU64::new(1),
            budget,
        })
    }

    pub(crate) fn script_cx(self: &Arc<Self>) -> ScriptCx {
        ScriptCx::new(
            Arc::clone(self) as Arc<dyn ScriptHost>,
            Arc::clone(&self.environment),
            None,
        )
    }

    pub(crate) fn records(&self) -> Vec<HostRecord> {
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
        Box::pin(async move { reply })
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
        Box::pin(async { Ok(Box::default()) })
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

pub(crate) fn failed(op: OpId, code: FailureCode, message: &str) -> OpOutcome {
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

pub(crate) fn native_op(op: NativeOp) -> OpId {
    OpId::Native(op)
}

pub(crate) fn test_services() -> Arc<dyn Services> {
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

pub(crate) fn tool_cx_approved(script: ScriptCx) -> ToolCx<'static> {
    ToolCx::for_test_approved(test_services()).with_script(script)
}

pub(crate) fn eval_args(code: &str) -> String {
    format!(
        "{{\"code\":{}}}",
        sonic_rs::to_string(code).expect("string JSON")
    )
}
