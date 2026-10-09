//! Boundary tests for the session script host over the real dispatch,
//! scope, admission, and evidence owners (R04 R07 E01 E05 E06).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dal_core::ext::{
    ExportId, ExportKind, NativeOp, OpId, OpSet, Phase, ReadView, SourceRow, ToolData,
};
use dal_core::{
    Budget, Caps, ClientId, Config, ConfigProduct, ContextItem, DenyReason, ModelId, ModelInfo,
    ModelRequest, ModelRoute, ModelToolSpec, Name, OnError, Purpose, RawJson, RequestParams,
    ScopeSpec, SessionId, ToolClass, ToolSpec, Visibility, Workspace,
};

use super::SessionScriptHost;
use crate::admission::Interpreters;
use crate::ext::generation::Generation;
use crate::ext::script::{
    CancelTarget, Collect, Entry, FailureCode, HostTerminal, Invocation, OpOutcome, OpRequest,
    OpValue, Parent, ScriptHost, Submit,
};
use crate::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use crate::ext::{
    BoxFuture, Caller, EventStream, ModelCx, ModelCxRuntime, ModelError, ModelRecord, PrivateTool,
    Scope, ScopeError,
};
use crate::host::Host;
use crate::session::backend::Backend;
use crate::{Env, Product, SessionRef};

struct DenyInferRuntime;

impl ModelCxRuntime for DenyInferRuntime {
    fn scope(&self, _spec: dal_core::ScopeSpec) -> Result<Scope, ScopeError> {
        Err(ScopeError::Cancelled)
    }

    fn infer<'a>(
        &'a self,
        _who: &'a Caller,
        _request: ModelRequest,
        _script: Arc<crate::session::script::SessionScriptHost>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'a, Result<dal_core::Inference, crate::error::ServiceError>> {
        Box::pin(async { Err(crate::error::ServiceError::Denied(DenyReason::NotInjected)) })
    }

    fn forward<'a>(
        &'a self,
        _request: ModelRequest,
        _private: &'a [PrivateTool],
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { Err(ModelError::SecondForward) })
    }
}

/// A registered export tool with a controllable wall delay (R03).
struct SlowTool {
    name: Name,
    spec: Arc<ToolSpec>,
    runs: Arc<AtomicUsize>,
    delay: Duration,
}

impl Tool for SlowTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let runs = Arc::clone(&self.runs);
        let delay = self.delay;
        Box::pin(async move {
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(delay).await;
            ToolOutcome::Ok(Box::new(crate::ext::ToolOutput::from_text("slow done")))
        })
    }
}

/// A registered export tool that emits a typed read view beside its text.
struct ViewTool {
    name: Name,
    spec: Arc<ToolSpec>,
}
struct ModelEntryHandler;

impl crate::ext::ModelHandler for ModelEntryHandler {
    fn uses(&self) -> OpSet {
        OpSet::parse(["models.infer"]).expect("model uses")
    }

    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { Err(ModelError::PrivateRounds) })
    }
}

impl Tool for ViewTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let view = ReadView {
                path: "sample.txt".into(),
                header: "[sample.txt@r1.1]".into(),
                rows: Box::new([
                    SourceRow {
                        line: 1,
                        text: "alpha line".into(),
                        complete: true,
                    },
                    SourceRow {
                        line: 2,
                        text: "beta line".into(),
                        complete: true,
                    },
                ]),
                truncated: false,
                provenance: None,
            };
            let mut output = crate::ext::ToolOutput::from_text("alpha line\nbeta line");
            output.data = Some(ToolData::Read(view));
            ToolOutcome::Ok(Box::new(output))
        })
    }
}

/// The started host with the product tools extension and the fixture exports.
struct Fixture {
    _host: Host,
    data: std::path::PathBuf,
    session: SessionId,
    backend: Arc<Backend>,
    interpreters: Arc<Interpreters>,
    generation: Arc<Generation>,
    runs: Arc<AtomicUsize>,
}

/// Starts one real session with the product tools and the fixture export.
async fn fixture(delay: Duration) -> Fixture {
    fixture_kind(delay, false).await
}

/// The fixture against a durable session whose store keeps a sidecar.
async fn durable_fixture(delay: Duration) -> Fixture {
    fixture_kind(delay, true).await
}

#[expect(
    clippy::too_many_lines,
    reason = "test fixture builds one real session"
)]
async fn fixture_kind(delay: Duration, durable: bool) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let fixture_source = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"ok\"},\
         {\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":\
         {\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":\
         null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\
         \"end_turn\"}]}\n";
    let fixture_path = data.join("script.jsonl");
    std::fs::write(&fixture_path, fixture_source).expect("fixture");
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture_path.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
    let approval = config.approval();
    let data_root = data.clone();
    let runs = Arc::new(AtomicUsize::new(0));
    let slow = Arc::new(SlowTool {
        name: Name::parse("fixture__slow").expect("tool name"),
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__slow").expect("tool name"),
            description: "counting fixture tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
            grammar: None,
        }),
        runs: Arc::clone(&runs),
        delay,
    });
    let export = crate::ext::generation::catalog::ExportSpec {
        id: ExportId {
            plugin: Name::parse("fixture").expect("plugin"),
            kind: ExportKind::Tool,
            local: Name::parse("slow").expect("local"),
        },
        uses: OpSet::parse(["tools.search"]).expect("uses"),
        input: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        description: "counting fixture export".into(),
    };
    let view_export = crate::ext::generation::catalog::ExportSpec {
        id: ExportId {
            plugin: Name::parse("fixture").expect("plugin"),
            kind: ExportKind::Tool,
            local: Name::parse("view").expect("local"),
        },
        uses: OpSet::EMPTY,
        input: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        description: "typed-view fixture export".into(),
    };
    let extra = crate::ext::ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)
        .expect("builder")
        .script_tool(slow, Visibility::Model, export)
        .script_tool(
            Arc::new(ViewTool {
                name: Name::parse("fixture__view").expect("tool name"),
                spec: Arc::new(ToolSpec {
                    name: Name::parse("fixture__view").expect("tool name"),
                    description: "typed-view fixture tool".into(),
                    parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                    grammar: None,
                }),
            }),
            Visibility::Model,
            view_export,
        )
        .model(ModelRecord {
            id: ModelId::parse("dalgona/script").expect("model id"),
            caps: Caps {
                context_window: Some(1024),
                thinking: Box::new([dal_core::ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(ModelEntryHandler),
            export: Some(ExportId {
                plugin: Name::parse("fixture").expect("plugin"),
                kind: ExportKind::Model,
                local: Name::parse("script").expect("model local"),
            }),
        })
        .build()
        .expect("fixture extension");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extra],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: std::collections::BTreeMap::new(),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let reference = if durable {
        SessionRef::New {
            workspace: workspace.clone(),
            name: None,
        }
    } else {
        SessionRef::Ephemeral {
            workspace: workspace.clone(),
        }
    };
    let _agent = host
        .open(reference, ClientId::new("probe"))
        .await
        .expect("open");
    let session = *host
        .state
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .next()
        .expect("session");
    let (handle, shared, broker, cancel, entry_workspace) = {
        let sessions = host
            .state
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sessions.get(&session).expect("entry");
        (
            entry.handle.clone(),
            Arc::clone(&entry.shared),
            Arc::clone(&entry.broker),
            entry.cancel.clone(),
            entry.workspace.clone(),
        )
    };
    // The fixture's job table is the data-plane's own; the actor keeps the
    // session table internally.
    let jobs = Arc::new(tokio::sync::Mutex::new(crate::jobs::JobTable::new()));
    let ephemeral = !durable;
    let tasks = crate::session::tasks::SessionTasks::new();
    let backend = Arc::new(Backend::new(crate::session::backend::BackendDeps {
        session,
        workspace: entry_workspace,
        host: Arc::clone(&host.state),
        shared: Arc::clone(&shared),
        initial_entries: Arc::from([]),
        broker: Arc::clone(&broker),
        handle,
        jobs: Arc::clone(&jobs),
        cancel: cancel.clone(),
        tasks: tasks.clone(),
    }));
    // The data-plane refuses nested calls until the session services are
    // published; build the same wiring the registry uses at session start.
    let grants = Arc::new(
        crate::ext::grants::GrantStore::with_runtime(
            data_root.clone(),
            Duration::from_secs(30),
            Arc::clone(&broker),
        )
        .expect("grants"),
    );
    let procs = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let rt = Arc::new(crate::session::rt::SessionRt::new(
        crate::session::rt::SessionRtDeps {
            workspace: workspace.clone(),
            shared: Arc::clone(&shared),
            initial_entries: Arc::from([]),
            scheme_store: Arc::clone(backend.scheme_store()),
            host: Arc::clone(&host.state),
            jobs: Arc::clone(&jobs),
            procs,
            env_snapshot: backend.env_snapshot().to_vec(),
            launcher: Ok(crate::proc::Launcher::Direct),
            approval,
            cancel: cancel.clone(),
            jobs_dir: backend.jobs_dir().to_path_buf(),
            tasks,
        },
    ));
    let services = Arc::new(crate::ext::services::SessionServices::new(
        crate::ext::services::SessionServicesDeps {
            grants,
            broker: Arc::clone(&broker),
            backend: Arc::clone(&backend) as Arc<dyn crate::ext::services::SessionBackend>,
            rt,
            mcp_client: None,
            generation: host.state.shared.generation.subscribe(),
            overlay: Arc::new(crate::ext::overlay::Overlay::default()),
            history: Arc::from([]),
            sites: std::collections::HashMap::new(),
            cancel: cancel.clone(),
            ask_timeout: Duration::from_secs(30),
            ephemeral,
            workspace: workspace.clone(),
        },
    ));
    backend.set_services(&services);
    let interpreters = Arc::clone(&host.state.shared.interpreters);
    let generation = host.state.shared.generation.borrow().clone();
    Fixture {
        _host: host,
        data: data_root,
        session,
        backend,
        interpreters,
        generation,
        runs,
    }
}

/// Mints a host and captures `allowed` as the environment (E01).
fn captured(fixture: &Fixture, allowed: &[&str]) -> Arc<SessionScriptHost> {
    let host = SessionScriptHost::for_generation(
        fixture.session,
        &fixture.backend,
        Arc::clone(&fixture.interpreters),
        Arc::clone(&fixture.generation),
    );
    let fingerprint = SessionScriptHost::policy_fingerprint(
        &dal_core::Policy {
            mode: dal_core::ApprovalMode::Ask,
            answerer_attached: false,
            allow_always: std::collections::BTreeSet::new(),
        },
        &OpSet::EMPTY,
    );
    let allowed = OpSet::parse(allowed.iter().copied()).expect("allowed set");
    host.capture(
        Arc::clone(&fixture.generation),
        allowed,
        Some(7),
        fingerprint,
        None,
        tokio_util::sync::CancellationToken::new(),
    )
    .expect("capture");
    host
}

/// Begins a root eval with an explicit `uses` list.
fn begin_eval(
    host: &SessionScriptHost,
    uses: Option<OpSet>,
) -> Result<Arc<Invocation>, HostTerminal> {
    host.begin(Parent::Root, Entry::Eval { uses })
}

/// Runs one synchronous operation and returns its outcome.
async fn call_op(
    host: &SessionScriptHost,
    inv: &Arc<Invocation>,
    op: OpId,
    args: &str,
) -> OpOutcome {
    host.call(inv, OpRequest::new(op, RawJson::parse(args).expect("args")))
        .await
}

/// The export id of the fixture tool.
fn slow_export() -> OpId {
    fixture_export("slow")
}

/// The export id of the typed-view fixture tool.
fn view_export() -> OpId {
    fixture_export("view")
}

fn model_export() -> ExportId {
    ExportId {
        plugin: Name::parse("fixture").expect("plugin"),
        kind: ExportKind::Model,
        local: Name::parse("script").expect("model local"),
    }
}

/// Builds one fixture export id.
fn fixture_export(local: &str) -> OpId {
    OpId::Export(ExportId {
        plugin: Name::parse("fixture").expect("plugin"),
        kind: ExportKind::Tool,
        local: Name::parse(local).expect("local"),
    })
}
fn scope_spec(limit: u16) -> ScopeSpec {
    ScopeSpec {
        limit,
        on_error: OnError::Cancel,
        budget: Budget::default(),
    }
}

fn model_request_args() -> RawJson {
    let request = ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Harness {
            id: "dalgon/normal".into(),
        },
        system: "".into(),
        tools: Arc::from(Vec::<ModelToolSpec>::new()),
        context: Arc::from(Vec::<ContextItem>::new()),
        params: RequestParams::default(),
        cache_key: None,
    };
    let encoded = sonic_rs::to_string(&request).expect("request encodes");
    RawJson::parse(&encoded).expect("request JSON validates")
}

#[tokio::test]
async fn root_eval_with_empty_uses_cannot_start_any_op() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let inv = begin_eval(&host, Some(OpSet::EMPTY)).expect("pure eval mints");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::ToolsRead),
        r#"{"path":"x"}"#,
    )
    .await;
    assert!(
        matches!(outcome, OpOutcome::Terminal(HostTerminal::Denied { .. })),
        "an empty ceiling denies every operation: {outcome:?}"
    );
}

#[tokio::test]
async fn model_export_begins_with_its_declared_uses() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["models.infer"]);
    let inv = host
        .begin(
            Parent::Root,
            Entry::Export {
                id: model_export(),
                phase: Phase::Model,
            },
        )
        .expect("registered model export begins");
    assert!(
        inv.allows(&OpId::Native(NativeOp::ModelsInfer)),
        "the model export declares models.infer"
    );
    assert!(
        !inv.allows(&OpId::Native(NativeOp::ModelsForward)),
        "the model export does not declare models.forward"
    );
}

#[tokio::test]
async fn models_infer_without_model_runtime_is_unavailable() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["models.infer"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = host
        .call(
            &inv,
            OpRequest::new(OpId::Native(NativeOp::ModelsInfer), model_request_args()),
        )
        .await;
    assert!(
        matches!(
            outcome,
            OpOutcome::Failed {
                failure: crate::ext::script::OpFailure {
                    code: FailureCode::Unavailable,
                    ..
                },
                ..
            }
        ),
        "ordinary script contexts have no model runtime fallback: {outcome:?}"
    );
}

#[tokio::test]
async fn models_infer_preserves_typed_service_denial() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["models.infer"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let mut script = host.attach(None).expect("script context attaches");
    script.model_runtime = Some(Arc::new(DenyInferRuntime));
    let req = OpRequest::new(OpId::Native(NativeOp::ModelsInfer), model_request_args())
        .with_model_context(&script);
    let outcome = host.call(&inv, req).await;
    assert!(
        matches!(
            outcome,
            OpOutcome::Terminal(HostTerminal::Denied {
                reason: DenyReason::NotInjected
            })
        ),
        "the typed service denial survives the op bridge: {outcome:?}"
    );
}

#[tokio::test]
async fn uses_outside_the_environment_fail_before_minting() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let outside = OpSet::parse(["tools.patch"]).expect("uses");
    let error = begin_eval(&host, Some(outside)).expect_err("scope refused");
    assert!(
        matches!(error, HostTerminal::ScopeExceeded { .. }),
        "uses outside A is scope_exceeded: {error}"
    );
}

#[tokio::test]
async fn call_outside_the_authority_is_denied_without_a_tool_run() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    // The fixture export is outside A entirely.
    let outcome = call_op(&host, &inv, slow_export(), "{}").await;
    assert!(matches!(
        outcome,
        OpOutcome::Terminal(HostTerminal::Denied { .. })
    ));
    assert_eq!(fx.runs.load(Ordering::SeqCst), 0, "no tool may run");
}

#[tokio::test]
async fn a_declared_export_runs_once_through_the_checked_path() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read", "tools.search", "tools.fixture.slow"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(&host, &inv, slow_export(), "{}").await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("the declared export runs: {outcome:?}");
    };
    assert!(matches!(value, OpValue::Json(_)));
    assert_eq!(fx.runs.load(Ordering::SeqCst), 1, "exactly one tool run");
}

#[tokio::test]
async fn a_view_reaches_the_host_as_typed_data() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.fixture.view"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(&host, &inv, view_export(), "{}").await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("the view export runs: {outcome:?}");
    };
    let OpValue::Data(ToolData::Read(view)) = value else {
        panic!("the tool carries its typed view: {value:?}");
    };
    assert_eq!(view.rows.len(), 2, "both complete rows reach the host");
}

#[tokio::test]
async fn a_backendless_native_op_fails_unavailable_without_effect() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::ToolsRead),
        r#"{"path":"x"}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = outcome else {
        panic!("a backendless op fails recoverably: {outcome:?}");
    };
    assert_eq!(failure.code, FailureCode::Unavailable);
}

#[tokio::test]
async fn a_service_op_reaches_the_typed_service() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["env.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::EnvRead),
        r#"{"key":"DAL_TEST_KEY_ABSENT_9F4C"}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("env.read routes to the env service: {outcome:?}");
    };
    let OpValue::Json(raw) = value else {
        panic!("env.read answers raw JSON: {value:?}");
    };
    assert_eq!(raw.as_str(), "null", "an absent key answers null");
}

#[tokio::test]
async fn a_service_op_rejects_unknown_arguments() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["env.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::EnvRead),
        r#"{"key":"HOME","bogus":1}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = outcome else {
        panic!("unknown keys fail the strict decode: {outcome:?}");
    };
    assert_eq!(failure.code, FailureCode::Failed);
    assert!(
        failure.message.contains("invalid arguments"),
        "the decode error names the contract: {failure:?}"
    );
}

#[tokio::test]
async fn a_service_op_replies_in_the_wire_shape() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["jobs.list"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(&host, &inv, OpId::Native(NativeOp::JobsList), "{}").await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("jobs.list routes to the jobs service: {outcome:?}");
    };
    let OpValue::Json(raw) = value else {
        panic!("jobs.list answers raw JSON: {value:?}");
    };
    assert_eq!(
        raw.as_str(),
        r#"{"type":"listed","value":[]}"#,
        "an empty job table answers the tagged wire shape"
    );
}

#[tokio::test]
async fn state_ops_compare_and_swap_through_the_actor() {
    let fx = durable_fixture(Duration::ZERO).await;
    let host = captured(&fx, &["state.read", "state.write", "state.delete"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let read = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = read else {
        panic!("state.read reaches the state owner: {read:?}");
    };
    let OpValue::State(record) = value else {
        panic!("state.read answers a typed record: {value:?}");
    };
    assert!(
        !record.present && record.value.is_none() && record.revision.get().get() == 1,
        "an absent key mints its first revision: {record:?}"
    );
    let write = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":41,"expected":1}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = write else {
        panic!("state.write with the minted revision commits: {write:?}");
    };
    let OpValue::State(record) = value else {
        panic!("state.write answers a typed record: {value:?}");
    };
    assert!(
        record.present
            && record.value.as_ref().map(dal_core::RawJson::as_str) == Some("41")
            && record.revision.get().get() == 2,
        "the committed record carries the next revision: {record:?}"
    );
    let stale = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":99,"expected":1}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = stale else {
        panic!("a stale revision is a catchable conflict: {stale:?}");
    };
    assert!(
        failure.message.contains("state revision conflict"),
        "the conflict keeps the R08 wording: {failure:?}"
    );
    assert_eq!(
        failure.code,
        crate::ext::script::FailureCode::Conflict,
        "a stale revision carries the retryable conflict code"
    );
    let recheck = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = recheck else {
        panic!("the conflicting write touched nothing: {recheck:?}");
    };
    let OpValue::State(record) = value else {
        panic!("state.read answers a typed record: {value:?}");
    };
    assert!(
        record.present
            && record.value.as_ref().map(dal_core::RawJson::as_str) == Some("41")
            && record.revision.get().get() == 2,
        "the rejected write left the committed value and revision: {record:?}"
    );
    let delete = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateDelete),
        r#"{"key":"counter","expected":2}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = delete else {
        panic!("state.delete with the minted revision tombstones: {delete:?}");
    };
    let OpValue::State(record) = value else {
        panic!("state.delete answers a typed record: {value:?}");
    };
    assert!(
        !record.present && record.value.is_none() && record.revision.get().get() == 3,
        "the tombstone mints a fresh revision a stale token cannot reuse: {record:?}"
    );
}

/// The session's `state` sidecar path under the fixture's data root:
/// `data/sessions/<workspace>/<session>/state`.
#[cfg(unix)]
fn state_sidecar(data: &std::path::Path) -> std::path::PathBuf {
    let sessions = std::fs::read_dir(data.join("sessions")).expect("sessions dir");
    for ws in sessions {
        let ws = ws.expect("ws entry").path();
        if let Some(sid) = std::fs::read_dir(ws).expect("session dir").next() {
            return sid.expect("sid entry").path().join("state");
        }
    }
    panic!("the durable session owns a state sidecar path")
}

/// A failed durable write must publish nothing: the map rolls back so the
/// same `expected` revision still commits once storage recovers (R08 —
/// memory may never run ahead of the sidecar).
#[cfg(unix)]
#[tokio::test]
async fn a_failed_sidecar_write_rolls_the_map_back() {
    use std::os::unix::fs::PermissionsExt;
    let fx = durable_fixture(Duration::ZERO).await;
    let host = captured(&fx, &["state.read", "state.write"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    // Mint the first revision so the session dir and state file exist.
    let first = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    let OpOutcome::Ok { .. } = first else {
        panic!("the first read minted a revision: {first:?}");
    };
    let dir = state_sidecar(&fx.data)
        .parent()
        .expect("session dir")
        .to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("read only");
    let failed = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":99,"expected":1}"#,
    )
    .await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
    let OpOutcome::Terminal(HostTerminal::Denied { reason, .. }) = failed else {
        panic!("a failed durable write denies the op: {failed:?}");
    };
    let DenyReason::Unavailable { what } = reason else {
        panic!("the sidecar is the unavailable piece: {reason:?}");
    };
    assert_eq!(what.as_ref(), "state");
    // The rolled-back op never minted a revision or wrote a value: the same
    // `expected` still commits, and no phantom record appears.
    let commit = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":99,"expected":1}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = commit else {
        panic!("the rolled-back map still accepts the original CAS: {commit:?}");
    };
    let OpValue::State(record) = value else {
        panic!("state.write answers a typed record: {value:?}");
    };
    assert!(
        record.present
            && record.value.as_ref().map(dal_core::RawJson::as_str) == Some("99")
            && record.revision.get().get() == 2,
        "the failed write minted nothing: {record:?}"
    );
}

/// A `state.read` of an existing key never touches the sidecar: the
/// session dir can be readable-but-unwritable and the read still answers
/// (R08 — reads mint nothing durable).
#[cfg(unix)]
#[tokio::test]
async fn a_read_on_an_existing_key_needs_no_write() {
    use std::os::unix::fs::PermissionsExt;
    let fx = durable_fixture(Duration::ZERO).await;
    let host = captured(&fx, &["state.read", "state.write"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let _ = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    let committed = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":41,"expected":1}"#,
    )
    .await;
    let OpOutcome::Ok { .. } = committed else {
        panic!("the record committed before the directory locked: {committed:?}");
    };
    let dir = state_sidecar(&fx.data)
        .parent()
        .expect("session dir")
        .to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("read only");
    let read = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
    let OpOutcome::Ok { value, .. } = read else {
        panic!("an existing-key read needs no write and still answers: {read:?}");
    };
    assert!(matches!(value, OpValue::State(..)));
}

#[tokio::test]
async fn state_ops_fail_closed_on_an_ephemeral_session() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["state.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateRead),
        r#"{"key":"counter"}"#,
    )
    .await;
    let OpOutcome::Terminal(HostTerminal::Denied { reason, .. }) = outcome else {
        panic!("an ephemeral session keeps the typed outcome: {outcome:?}");
    };
    let DenyReason::Unavailable { what } = reason else {
        panic!("the state store is the unavailable piece: {reason:?}");
    };
    assert_eq!(what.as_ref(), "state");
}

#[tokio::test]
async fn a_state_op_rejects_a_zero_revision() {
    let fx = durable_fixture(Duration::ZERO).await;
    let host = captured(&fx, &["state.write"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::StateWrite),
        r#"{"key":"counter","value":1,"expected":0}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = outcome else {
        panic!("revision zero is an argument failure: {outcome:?}");
    };
    assert!(
        failure.message.contains("invalid arguments"),
        "the decode error names the contract: {failure:?}"
    );
}

#[tokio::test]
async fn the_state_namespace_derives_from_the_caller() {
    use crate::ext::{Caller, CallerKind};
    use dal_core::StateNs;
    let fx = durable_fixture(Duration::ZERO).await;
    fx.backend
        .script_services()
        .expect("the fixture published services");
    let cell = Caller::new(
        Name::parse("fixture").expect("plugin"),
        dal_core::Origin::User,
        dal_core::ServiceSet::EMPTY,
        std::num::NonZeroU32::MIN,
        CallerKind::Cell { approved: false },
        None,
    );
    assert_eq!(
        crate::ext::services::SessionServices::state_ns(&cell),
        StateNs::Eval
    );
    // A minted caller carries its extension's declared state_version: the
    // namespace binds to the caller's generation snapshot, not a later reload.
    let minted = std::num::NonZeroU32::new(7).expect("nonzero");
    let who = Caller::new(
        Name::parse("fixture").expect("plugin"),
        dal_core::Origin::User,
        dal_core::ServiceSet::EMPTY,
        minted,
        CallerKind::Tool,
        None,
    );
    assert_eq!(
        crate::ext::services::SessionServices::state_ns(&who),
        StateNs::Plugin {
            origin: dal_core::Origin::User,
            plugin: Name::parse("fixture").expect("plugin"),
            version: minted,
        },
        "a tool caller owns its plugin's isolated namespace"
    );
}

#[tokio::test]
async fn agents_start_refuses_a_tools_allowlist() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["agents.start"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::AgentsStart),
        r#"{"prompt":"work","tools":["bash"]}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = outcome else {
        panic!("an unhonorable allowlist fails rather than widens: {outcome:?}");
    };
    assert!(
        failure.message.contains("does not accept tools"),
        "the refusal names the unsupported field: {failure:?}"
    );
}

/// A child workspace spelled through an in-root symlink resolves outside
/// it; containment must be checked on canonical paths or `agents.start`
/// widens into tool access across the filesystem.
#[cfg(unix)]
#[tokio::test]
async fn agents_start_cannot_escape_the_workspace_through_a_link() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["agents.start"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outside = fx.data.join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    // The fixture's tempdir root is reaped with it; recreate the workspace
    // before planting the link inside it.
    let workspace = fx.backend.workspace().as_path().to_path_buf();
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let link = workspace.join("link");
    std::os::unix::fs::symlink(&outside, &link).expect("workspace link");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::AgentsStart),
        &format!(r#"{{"prompt":"work","workspace":"{}"}}"#, link.display()),
    )
    .await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("agents.start answers a typed reply: {outcome:?}");
    };
    let OpValue::Json(raw) = value else {
        panic!("agents.start answers raw JSON: {value:?}");
    };
    let reply = raw.as_str();
    assert!(
        reply.contains(r#""type":"cancelled""#),
        "a link resolving outside the workspace is refused: {reply}"
    );
}

#[derive(serde::Deserialize)]
struct AgentsStarted {
    #[serde(rename = "type")]
    kind: Box<str>,
    value: AgentsStartedValue,
}

#[derive(serde::Deserialize)]
struct AgentsStartedValue {
    id: Box<str>,
}

#[derive(serde::Deserialize)]
struct AgentsAwaited {
    #[serde(rename = "type")]
    kind: Box<str>,
    value: AgentsAwaitedValue,
}

#[derive(serde::Deserialize)]
struct AgentsAwaitedValue {
    report: AgentsAwaitedReport,
}

#[derive(serde::Deserialize)]
struct AgentsAwaitedReport {
    stop: Box<str>,
}

/// A runnable `agents.start` must answer `started` and let its child finish:
/// submitting any command that the fold rejects mid-turn (e.g. a rename while
/// the accepted prompt is still running) would close the child instead and
/// turn every start into `cancelled`.
#[tokio::test]
async fn agents_start_returns_started_and_the_child_completes() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["agents.start", "agents.wait"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    // The fixture's tempdir root is reaped with it; the durable child needs
    // its workspace and store root to exist on disk again.
    std::fs::create_dir_all(&fx.data).expect("data dir");
    let workspace = fx.backend.workspace().as_path().to_path_buf();
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::AgentsStart),
        r#"{"name":"probe","prompt":"reply"}"#,
    )
    .await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("agents.start answers a typed reply: {outcome:?}");
    };
    let OpValue::Json(raw) = value else {
        panic!("agents.start answers raw JSON: {value:?}");
    };
    let started: AgentsStarted = raw.decode_as().expect("started reply decodes");
    assert_eq!(
        started.kind.as_ref(),
        "started",
        "a runnable child starts: {raw:?}"
    );
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::AgentsWait),
        &format!(r#"{{"id":"{}"}}"#, started.value.id),
    )
    .await;
    let OpOutcome::Ok { value, .. } = outcome else {
        panic!("agents.await answers a typed reply: {outcome:?}");
    };
    let OpValue::Json(raw) = value else {
        panic!("agents.await answers raw JSON: {value:?}");
    };
    let report: AgentsAwaited = raw.decode_as().expect("await reply decodes");
    assert_eq!(
        report.kind.as_ref(),
        "await",
        "the child completes: {raw:?}"
    );
    assert_eq!(
        report.value.report.stop.as_ref(),
        "end_turn",
        "the report carries the durable terminal stop: {raw:?}"
    );
}

#[tokio::test]
async fn an_unbacked_native_op_still_fails_unavailable() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["mcp.call"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::McpCall),
        r#"{"server":"fixture","tool":"probe","arguments":{}}"#,
    )
    .await;
    let OpOutcome::Failed { failure, .. } = outcome else {
        panic!("mcp.call has no backend in this generation: {outcome:?}");
    };
    assert_eq!(failure.code, FailureCode::Unavailable);
}

#[tokio::test]
async fn scope_results_come_back_in_submission_order() {
    let fx = fixture(Duration::from_millis(250)).await;
    let host = captured(&fx, &["tools.fixture.slow", "tools.fixture.view"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let scope = host.open_scope(&inv, scope_spec(2)).expect("scope opens");
    let slow = host.submit(
        &inv,
        scope,
        OpRequest::new(slow_export(), RawJson::parse("{}").expect("args")),
    );
    let first = match slow {
        Submit::Queued(task) => task,
        other => panic!("the slow task queues: {other:?}"),
    };
    let view = host.submit(
        &inv,
        scope,
        OpRequest::new(view_export(), RawJson::parse("{}").expect("args")),
    );
    let second = match view {
        Submit::Queued(task) => task,
        other => panic!("the view task queues: {other:?}"),
    };
    // The search finishes first; delivery still follows submission order.
    let collected = host
        .collect(&inv, Collect::Seal(scope))
        .await
        .expect("collection completes");
    let ids: Vec<_> = collected.iter().map(|(task, _)| *task).collect();
    assert_eq!(ids, vec![first, second], "submission order");
    assert!(
        matches!(collected[0].1, OpOutcome::Ok { .. }),
        "the slow export succeeded: {:?}",
        collected[0].1
    );
    let OpOutcome::Ok { value, .. } = &collected[1].1 else {
        panic!("the view task succeeded: {:?}", collected[1].1);
    };
    assert!(
        matches!(value, OpValue::Data(_)),
        "the view carries its page"
    );
}

#[tokio::test]
async fn owner_cancel_is_recoverable_while_root_cancel_is_terminal() {
    let fx = fixture(Duration::from_millis(400)).await;
    let host = captured(&fx, &["tools.fixture.slow", "tools.fixture.view"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let scope = host.open_scope(&inv, scope_spec(1)).expect("scope opens");
    let submitted = host.submit(
        &inv,
        scope,
        OpRequest::new(slow_export(), RawJson::parse("{}").expect("args")),
    );
    let first = match submitted {
        Submit::Queued(task) => task,
        other => panic!("the slow task queues: {other:?}"),
    };
    let queued = host.submit(
        &inv,
        scope,
        OpRequest::new(view_export(), RawJson::parse("{}").expect("args")),
    );
    let second = match queued {
        Submit::Queued(task) => task,
        other => panic!("the view task queues behind the limit: {other:?}"),
    };
    host.cancel(&inv, CancelTarget::Task(second));
    let collected = host
        .collect(&inv, Collect::Task(second))
        .await
        .expect("the cancelled task delivers");
    let OpOutcome::Failed { failure, .. } = &collected[0].1 else {
        panic!(
            "an owner cancel is a recoverable result: {:?}",
            collected[0].1
        );
    };
    assert_eq!(failure.code, FailureCode::Cancelled);
    // An external root cancellation is terminal and cannot be swallowed.
    inv.cancel().cancel();
    let error = host
        .collect(&inv, Collect::Task(first))
        .await
        .expect_err("root cancel terminates");
    assert!(matches!(error, HostTerminal::Cancelled));
}

#[tokio::test]
async fn finish_cancels_owned_scopes_and_reports_outstanding() {
    let fx = fixture(Duration::from_secs(30)).await;
    let host = captured(&fx, &["tools.fixture.slow"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let scope = host.open_scope(&inv, scope_spec(1)).expect("scope opens");
    let submitted = host.submit(
        &inv,
        scope,
        OpRequest::new(slow_export(), RawJson::parse("{}").expect("args")),
    );
    let first = match submitted {
        Submit::Queued(task) => task,
        other => panic!("the slow task queues: {other:?}"),
    };
    // One collect branch issues the task and keeps polling while the
    // finish branch cancels the invocation under it (E07).
    let ((), cleanup) = tokio::join!(
        async {
            let _ = host.collect(&inv, Collect::Task(first)).await;
        },
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            host.finish(&inv).await
        },
    );
    assert!(!cleanup.complete, "the unresolved native call is reported");
    assert_eq!(cleanup.outstanding.len(), 1, "exactly the issued call");
    assert!(
        cleanup.outstanding[0].as_str().starts_with("scope-"),
        "the outstanding id is the submission-minted call id"
    );
}

#[tokio::test]
async fn nested_eval_and_nested_scripted_export_are_rejected() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let eval = begin_eval(&host, None).expect("eval mints");
    let error = host
        .begin(Parent::Of(&eval), Entry::Eval { uses: None })
        .expect_err("nested eval refused");
    assert!(matches!(error, HostTerminal::NestedEval));
    let export = host
        .begin(
            Parent::Root,
            Entry::Export {
                id: match slow_export() {
                    OpId::Export(id) => id,
                    OpId::Native(_) => unreachable!(),
                },
                phase: Phase::Tool,
            },
        )
        .expect("export mints");
    let error = host
        .begin(
            Parent::Of(&export),
            Entry::Export {
                id: match slow_export() {
                    OpId::Export(id) => id,
                    OpId::Native(_) => unreachable!(),
                },
                phase: Phase::Tool,
            },
        )
        .expect_err("nested scripted export refused");
    assert!(matches!(error, HostTerminal::NestedScriptExport));
}

#[tokio::test]
async fn eval_entry_intersects_the_export_declaration_with_the_ceiling() {
    let fx = fixture(Duration::ZERO).await;
    // A omits tools.search, a primitive the fixture export declares.
    let host = captured(&fx, &["tools.read", "tools.fixture.slow"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(&host, &inv, slow_export(), "{}").await;
    assert!(
        matches!(
            outcome,
            OpOutcome::Terminal(HostTerminal::IncompleteScope { .. })
        ),
        "missing primitives refuse the entry: {outcome:?}"
    );
}

#[tokio::test]
async fn adoption_refuses_references_outside_the_parent_cutoff() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["tools.read"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let error = host.adopt(&inv, "r1.1").expect_err("foreign reference");
    assert_eq!(error.code, FailureCode::ObservationUnavailable);
}

#[tokio::test]
async fn a_get_fetch_needs_no_headers_or_body() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["net.fetch"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::NetFetch),
        r#"{"method":"GET","url":"http://127.0.0.1:1/"}"#,
    )
    .await;
    if let OpOutcome::Failed { failure, .. } = &outcome {
        assert!(
            !failure.message.contains("invalid arguments"),
            "headers and body are optional on a GET: {failure:?}"
        );
    }
}

#[tokio::test]
async fn label_only_choices_open_a_select_question() {
    let fx = fixture(Duration::ZERO).await;
    let host = captured(&fx, &["ask.select"]);
    let inv = begin_eval(&host, None).expect("eval inherits A");
    let outcome = call_op(
        &host,
        &inv,
        OpId::Native(NativeOp::AskSelect),
        r#"{"prompt":"pick","options":[{"label":"a"},{"label":"b"}],"multi":false}"#,
    )
    .await;
    if let OpOutcome::Failed { failure, .. } = &outcome {
        assert!(
            !failure.message.contains("invalid arguments"),
            "choice description is optional: {failure:?}"
        );
    }
}
