//! Session service gates over scripted backends.
//!
//! The broker is real: tests open questions through [`SessionServices`] and
//! answer them by driving the broker slots, exactly like the future front
//! ends will. The backend and the run launcher are scripts.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::future::Future;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::SkillRecord;
use dal_core::ext::{McpBlock, McpDeclaration, McpEndpoint, McpLaunch, McpServerDecl};
use dal_core::{
    AgentsOp, AgentsReply, Answer, ApprovalMode, Budget, CallId, ClientId, DenyReason, EntryId,
    Gen, Inference, JobId, JobsOp, JobsReply, JournalPart, Mode, ModelRequest, ModelRoute, Name,
    OnError, Origin, Owner, PrivateEffect, PrivateEffectId, PrivateLoopId, PrivateOutcome, Purpose,
    Question, RawJson, Record, RequestParams, ResolutionCause, ScopeSpec, Service, ServiceSet,
    Session, SessionId, SidecarName, SidecarOp, Site, StateError, StateOp, StateRecord,
    ThinkingLevel, TurnId, TurnOp, TurnOpReply, Workspace,
};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::SessionServicesDeps;
use super::{Caller, CallerKind, ServiceFuture, Services, SessionBackend, SessionServices};
use crate::Broker;
use crate::broker::Resolution;
use crate::error::ServiceError;
use crate::ext::Doc;
use crate::ext::generation::{Generation, ValidatedExtensions};
use crate::ext::grants::GrantStore;
use crate::ext::tool::{Approved, ToolCxRuntime};
use crate::ext::{Extension, ExtensionBuilder};
use crate::ext::{Scope, ScopeError};
use crate::proc::Proc;
use crate::session::backend::private_owner_of;
use crate::session::shared::Shared;

/// One canned reply per backend operation, in call order.
#[derive(Default)]
struct FakeBackend {
    agents_reply: Mutex<Option<AgentsReply>>,
    jobs_reply: Mutex<Option<JobsReply>>,
    turn_reply: Mutex<Option<TurnOpReply>>,
    private_outcomes_reply: Mutex<Vec<PrivateEffect>>,
    sidecar_value: Mutex<Option<Vec<u8>>>,
    /// Every state operation the backend received, exactly as received.
    state_ops: Mutex<Vec<StateOp>>,
    /// The scripted state outcome; unscripted reads answer `Unavailable`.
    state_reply: Mutex<Option<Result<StateRecord, StateError>>>,
    infer_reply: Mutex<Option<Inference>>,
    seen_env: Mutex<Vec<String>>,
    env_value: Mutex<Option<String>>,
    rows: Mutex<Vec<crate::ext::ExtRecord>>,
    blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    updates: Mutex<Vec<dal_core::UpdateKind>>,
    headless: std::sync::atomic::AtomicBool,
    seen_infer_callers: Mutex<Vec<Caller>>,
    /// Every path `fs_read` reached; a gate that refuses first leaves it empty.
    seen_reads: Mutex<Vec<String>>,
}

impl SessionBackend for FakeBackend {
    fn append_record(&self, ext: &Name, kind: &str, body: RawJson) -> ServiceFuture<'_, EntryId> {
        let mut rows = self.rows.lock().unwrap();
        rows.push(crate::ext::ExtRecord {
            ext: ext.clone(),
            kind: kind.into(),
            body,
        });
        let id = EntryId::new(NonZeroU64::new(rows.len() as u64).unwrap());
        Box::pin(async move { Ok(id) })
    }

    fn ext_records(&self) -> Arc<[crate::ext::ExtRecord]> {
        Arc::from(self.rows.lock().unwrap().clone())
    }
    fn private_outcomes(&self, _caller: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
        let rows = self.private_outcomes_reply.lock().unwrap().clone();
        Box::pin(async move { Ok(rows) })
    }
    fn blob_put(&self, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        let digest = *blake3::hash(&bytes).as_bytes();
        self.blobs.lock().unwrap().entry(digest).or_insert(bytes);
        Box::pin(async move { Ok(digest) })
    }

    fn blob_get(&self, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let bytes = self.blobs.lock().unwrap().get(&digest).cloned();
        Box::pin(async move { Ok(bytes) })
    }

    fn fs_read(&self, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        self.seen_reads.lock().unwrap().push(path.to_owned());
        Box::pin(async move { Ok(Some(b"served".to_vec())) })
    }

    fn fs_write(&self, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        Box::pin(async move { unreachable!("this test never writes files") })
    }

    fn net(&self, _req: dal_core::FetchRequest) -> ServiceFuture<'_, dal_core::FetchResponse> {
        Box::pin(async move { unreachable!("this test never fetches") })
    }

    fn agents(&self, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        let reply = self
            .agents_reply
            .lock()
            .unwrap()
            .clone()
            .expect("agents reply is scripted");
        Box::pin(async move { Ok(reply) })
    }

    fn jobs(&self, _owner: &Name, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        let reply = self
            .jobs_reply
            .lock()
            .unwrap()
            .clone()
            .expect("jobs reply is scripted");
        Box::pin(async move { Ok(reply) })
    }

    fn scheme(&self, _caller: &Caller, _uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        Box::pin(async move { Ok(None) })
    }

    fn sidecar_artifact(
        &self,
        _job: dal_core::JobId,
        _file: dal_core::ArtifactFile,
        _bytes: Vec<u8>,
    ) -> ServiceFuture<'_, ()> {
        Box::pin(async move { Ok(()) })
    }

    fn turn(&self, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        let reply = self
            .turn_reply
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .expect("turn reply is scripted");
        Box::pin(async move { Ok(reply) })
    }

    fn sidecar_read(&self, _ext: &Name, _name: &SidecarName) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let value = self.sidecar_value.lock().unwrap().clone();
        Box::pin(async move { Ok(value) })
    }

    fn sidecar_write(
        &self,
        _ext: &Name,
        name: &SidecarName,
        bytes: Vec<u8>,
    ) -> ServiceFuture<'_, ()> {
        assert!(
            !name.as_str().is_empty(),
            "sidecar name travels with the write"
        );
        assert!(!bytes.is_empty(), "sidecar bytes travel with the write");
        Box::pin(async move { Ok(()) })
    }

    fn state(&self, op: StateOp) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        self.state_ops.lock().unwrap().push(op);
        let reply = self
            .state_reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Err(StateError::Unavailable));
        Box::pin(async move { Ok(reply) })
    }

    fn model_ready(&self, _model: ModelRoute) -> ServiceFuture<'_, Option<ModelRoute>> {
        Box::pin(async move { unreachable!("this test never probes model readiness") })
    }

    fn infer(&self, caller: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        self.seen_infer_callers.lock().unwrap().push(caller.clone());
        let reply = self
            .infer_reply
            .lock()
            .unwrap()
            .clone()
            .expect("infer is scripted");
        Box::pin(async move { Ok(reply) })
    }

    fn infer_stream(
        &self,
        caller: &Caller,
        _req: ModelRequest,
    ) -> ServiceFuture<'_, dal_provider::EventStream> {
        self.seen_infer_callers.lock().unwrap().push(caller.clone());
        Box::pin(async move {
            Ok(dal_provider::EventStream::new(
                futures::stream::empty::<
                    Result<dal_provider::StreamEvent, dal_provider::ProviderError>,
                >(),
                || {},
            ))
        })
    }

    fn call_tool(
        &self,
        _name: &str,
        _args: &dal_core::RawJson,
    ) -> ServiceFuture<'_, crate::ext::tool::ToolOutcome> {
        Box::pin(async move { unreachable!("this test never calls tools") })
    }

    fn publish_update(&self, update: dal_core::UpdateKind) {
        self.updates.lock().unwrap().push(update);
    }
    fn request_opened(&self, _request: dal_core::Request) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn answerer_attached(&self) -> bool {
        !self.headless.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn notify(&self, _notice: dal_core::Notice) {
        unreachable!("this test never notifies")
    }

    fn env(&self, key: &str) -> Option<String> {
        self.seen_env.lock().unwrap().push(key.to_owned());
        self.env_value.lock().unwrap().clone()
    }
}

/// One scripted authorization answer per `run` call, in call order.
enum AuthorizeScript {
    /// The ladder asks and the front end approves with this scope.
    AskThenApprove {
        prefix: Vec<OsString>,
        roots: Vec<PathBuf>,
    },
    /// The ladder finds a live grant and asks nothing.
    Covered {
        prefix: Vec<OsString>,
        roots: Vec<PathBuf>,
    },
    /// The ladder refuses without asking.
    Deny(DenyReason),
}

struct SpawnRecord {
    argv: Vec<OsString>,
    cwd: PathBuf,
    env: Vec<(OsString, OsString)>,
    stdout_prefix_limit: usize,
}

type SpawnLog = Mutex<Vec<SpawnRecord>>;

struct Spawned<T>(JoinSet<T>);

fn spawn<F>(future: F) -> Spawned<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut set = JoinSet::new();
    set.spawn(future);
    Spawned(set)
}

impl<T: 'static> Spawned<T> {
    async fn join(mut self) -> T {
        self.0
            .join_next()
            .await
            .expect("one task was spawned")
            .expect("the task completed")
    }
}

struct FakeRt {
    previews: Mutex<Vec<dal_core::Preview>>,
    ladder_asks: Mutex<usize>,
    script: Mutex<VecDeque<AuthorizeScript>>,
    decisions: Mutex<VecDeque<dal_core::Decision>>,
    spawns: SpawnLog,
    workspace: Workspace,
    cancel: CancellationToken,
}

impl Default for FakeRt {
    fn default() -> Self {
        Self {
            previews: Mutex::new(Vec::new()),
            ladder_asks: Mutex::new(0),
            script: Mutex::new(VecDeque::new()),
            decisions: Mutex::new(VecDeque::new()),
            spawns: Mutex::new(Vec::new()),
            workspace: Workspace::new(std::env::temp_dir()).expect("test workspace"),
            cancel: CancellationToken::new(),
        }
    }
}

impl ToolCxRuntime for FakeRt {
    fn decide_run(&self) -> dal_core::Decision {
        self.decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(dal_core::Decision::Allow)
    }

    fn authorize_approved(
        &self,
        call: &CallId,
        preview: dal_core::Preview,
        cancel: &CancellationToken,
    ) -> crate::ext::BoxFuture<'_, Result<Approved, DenyReason>> {
        self.authorize(call, preview, cancel)
    }
    fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    fn authorize(
        &self,
        call: &CallId,
        preview: dal_core::Preview,
        _cancel: &CancellationToken,
    ) -> crate::ext::BoxFuture<'_, Result<Approved, DenyReason>> {
        self.previews.lock().unwrap().push(preview);
        let next = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("authorize is scripted");
        let mut asks = self.ladder_asks.lock().unwrap();
        let verdict = match next {
            AuthorizeScript::AskThenApprove { prefix, roots } => {
                *asks += 1;
                Ok((prefix, roots))
            }
            AuthorizeScript::Covered { prefix, roots } => Ok((prefix, roots)),
            AuthorizeScript::Deny(reason) => Err(reason),
        };
        drop(asks);
        let call = CallId::new(call.as_str());
        Box::pin(async move {
            match verdict {
                Ok((prefix, roots)) => Ok(Approved::new(
                    call,
                    None,
                    prefix.into_boxed_slice(),
                    roots.into_boxed_slice(),
                    None,
                )),
                Err(reason) => Err(reason),
            }
        })
    }

    fn spawn(
        &self,
        argv: &[OsString],
        opts: crate::proc::SpawnOpts,
        _approved: Approved,
    ) -> Result<Proc, crate::error::ToolError> {
        self.spawns.lock().unwrap().push(SpawnRecord {
            argv: argv.to_vec(),
            cwd: opts.cwd,
            env: opts.env,
            stdout_prefix_limit: opts.stdout_prefix_limit,
        });
        Err(crate::error::ToolError::Spawn {
            path: argv.first().map(PathBuf::from).unwrap_or_default(),
            source: std::io::Error::other("the scripted launcher refuses execution"),
        })
    }

    fn detach(&self, _proc: Proc) -> JobId {
        unreachable!("this test never detaches")
    }

    fn resolve(
        &self,
        _uri: &str,
        _context: crate::ext::scheme::SchemeResolveContext<'_>,
    ) -> crate::ext::BoxFuture<'_, Result<Doc, crate::error::ToolError>> {
        Box::pin(async move { unreachable!("this test never resolves") })
    }
}

struct Fixture {
    services: Arc<SessionServices>,
    broker: Arc<Broker>,
    /// The claim inbox the services share with the session handle; tests
    /// drain it the way the actor would.
    inbox: Arc<crate::session::ResolutionInbox>,
    backend: Arc<FakeBackend>,
    rt: Arc<FakeRt>,
    temp: tempfile::TempDir,
    cancel: CancellationToken,
    turn_token: CancellationToken,
    generation: watch::Sender<Arc<Generation>>,
}

fn generation_of(extensions: Vec<Extension>) -> Arc<Generation> {
    let validated = ValidatedExtensions::validate(extensions, None).unwrap();
    Arc::new(Generation::build(validated))
}

fn fixture(ask_timeout: Duration) -> Fixture {
    assemble(ask_timeout, false)
}

fn ephemeral_fixture(ask_timeout: Duration) -> Fixture {
    assemble(ask_timeout, true)
}

fn assemble(ask_timeout: Duration, ephemeral: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let broker = Arc::new(Broker::new());
    let grants = Arc::new(
        GrantStore::with_runtime_scoped(
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
            Duration::from_secs(120),
            Arc::clone(&broker),
        )
        .unwrap(),
    );
    let inbox = Arc::new(crate::session::ResolutionInbox::new(Arc::clone(&broker)));
    let backend = Arc::new(FakeBackend::default());
    let rt = Arc::new(FakeRt::default());
    let cancel = CancellationToken::new();
    let workspace = Workspace::new(temp.path().to_path_buf()).unwrap();
    let mut sites = HashMap::new();
    sites.insert(
        "focus".parse::<Name>().unwrap(),
        Some(Site {
            path: PathBuf::from("plugin/star"),
            line: 3,
            col: 7,
        }),
    );
    let (generation, generation_rx) = watch::channel(generation_of(Vec::new()));
    let overlay = Arc::new(crate::ext::overlay::Overlay::default());
    let turn_token = CancellationToken::new();
    let services = Arc::new(SessionServices::new(SessionServicesDeps {
        grants,
        broker: Arc::clone(&broker),
        backend: Arc::<FakeBackend>::clone(&backend),
        rt: Arc::<FakeRt>::clone(&rt),
        mcp_client: None,
        generation: generation_rx,
        overlay: Arc::clone(&overlay),
        history: Arc::from(["first".to_owned(), "second".to_owned()]),
        sites,
        turn_cancel: {
            let live = turn_token.clone();
            Arc::new(move |_| Some(live.clone()))
        },
        cancel: cancel.clone(),
        resolutions: Arc::clone(&inbox),
        ask_timeout,
        ephemeral,
        workspace,
    }));
    Fixture {
        services,
        broker,
        inbox,
        backend,
        rt,
        temp,
        cancel,
        turn_token,
        generation,
    }
}

fn caller(ext: &str, inject: &[&str], turn: Option<TurnId>) -> Caller {
    Caller::new(
        ext.parse::<Name>().unwrap(),
        Origin::User,
        ServiceSet::from_names(inject.iter().copied()).unwrap(),
        std::num::NonZeroU32::MIN,
        CallerKind::Handler,
        turn,
    )
    .with_test_digest()
}

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::new(1).unwrap())
}

/// Answers the oldest open broker question.
fn answer_next(broker: &Broker, answer: Answer) {
    let id = broker
        .open_requests()
        .into_iter()
        .next()
        .expect("an open request")
        .id;
    let resolved = broker
        .answer(id, answer, ClientId::new("test-front-end"))
        .expect("answer sends");
    // The helper stands in for the actor's journal-then-release step.
    broker.release(&resolved);
    broker
        .state
        .lock()
        .unwrap()
        .open_order
        .retain(|open| *open != id);
}

/// Cancels the oldest open broker question the way a turn cancel does: the
/// broker resolves it as cancelled and the actor's release delivers that
/// resolution to the waiter.
fn cancel_next(broker: &Broker, turn: TurnId) {
    let id = broker
        .open_requests()
        .into_iter()
        .next()
        .expect("an open request")
        .id;
    // The fake backend's `request_opened` is a no-op, so model the actor's
    // fold of the open: `resolve_turn` parks unreported claims until
    // `report_opened` hands them over.
    let _ = broker.report_opened(id);
    let resolved = broker.resolve_turn(turn, ResolutionCause::Cancelled);
    assert!(
        resolved.iter().any(|item| item.request.id == id),
        "the turn cancel resolves the open question"
    );
    for item in &resolved {
        assert_eq!(item.cause, ResolutionCause::Cancelled);
        broker.release(item);
    }
}

fn open_count(broker: &Broker) -> usize {
    broker.open_requests().len()
}

/// Waits until the services open one unresolved broker question.
async fn await_open(broker: &Broker) {
    for _ in 0..100 {
        if open_count(broker) > 0 {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("no question opened");
}

/// Waits until the services hold two unresolved broker questions at once.
async fn await_second_open(broker: &Broker) {
    for _ in 0..100 {
        if open_count(broker) > 1 {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("no second question opened");
}

fn model_request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::synthetic("pool/fast").unwrap(),
        system: "".into(),
        tools: Vec::new().into(),
        context: Vec::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

#[tokio::test]
async fn not_injected_does_not_consult_grants() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &["ask"], Some(turn()));
    let error = fx.services.fs_read(&who, "a.txt").await.unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Denied(DenyReason::NotInjected)
    ));
    assert_eq!(
        fx.services.inject_remedy(&who, Service::FsRead),
        "service \"fs.read\" is not declared for plugin \"focus\"; \
         declare the operation in the tool's `uses` (the v1 contract) at plugin/star:3:7 \
         and approve the new grant when asked",
    );
    assert_eq!(open_count(&fx.broker), 0, "no grant question may open");
    let ghost = caller("ghost", &[], Some(turn()));
    assert!(
        fx.services.inject_remedy(&ghost, Service::Net).ends_with(
            "declare the operation in the tool's `uses` (the v1 contract) at unknown \
             and approve the new grant when asked",
        ),
        "a missing span renders as unknown",
    );
}

/// The session's `jobs/` directory is outside the workspace, so an absolute
/// `fs.read` path is a Builtin privilege: the guard reads the exec tool's
/// spill log by the absolute path its footer names. Any other origin holding
/// the same service is refused before the backend sees the path, with the
/// refusal a path outside the workspace always had; its relative reads are
/// untouched.
#[tokio::test]
async fn an_absolute_fs_read_path_reaches_the_backend_only_for_a_builtin_caller() {
    let fx = fixture(Duration::from_secs(30));
    let absolute = fx.temp.path().join("jobs").join("a.log");
    let absolute = absolute.to_str().expect("utf-8 path");
    let reader = |ext: &str, origin: Origin, kind: CallerKind| {
        Caller::new(
            ext.parse::<Name>().unwrap(),
            origin,
            ServiceSet::from_names(["fs.read"]).unwrap(),
            std::num::NonZeroU32::MIN,
            kind,
            Some(turn()),
        )
        .with_test_digest()
    };
    let builtin = reader("guard", Origin::Builtin, CallerKind::Hook);
    // Cells skip the grant store, so the origin check is the only gate under test.
    let user = reader("focus", Origin::User, CallerKind::Cell { approved: true });
    let bundled = reader(
        "focus",
        Origin::Bundled,
        CallerKind::Cell { approved: true },
    );

    assert_eq!(
        fx.services.fs_read(&builtin, absolute).await.unwrap(),
        Some(b"served".to_vec()),
        "a Builtin caller reaches the backend with the absolute path"
    );
    for refused in [&user, &bundled] {
        let error = fx.services.fs_read(refused, absolute).await.unwrap_err();
        assert!(
            matches!(
                error,
                ServiceError::Denied(DenyReason::OutOfScope { ref what })
                    if what.contains(absolute)
            ),
            "an absolute path is out of scope for a non-Builtin caller, got {error:?}"
        );
    }
    assert_eq!(
        fx.backend.seen_reads.lock().unwrap().as_slice(),
        [absolute],
        "only the Builtin read reached the backend"
    );
    assert_eq!(
        fx.services.fs_read(&user, "a.txt").await.unwrap(),
        Some(b"served".to_vec()),
        "a workspace-relative read is unchanged for every origin"
    );
}

#[tokio::test]
async fn open_asks_counts_only_user_questions_and_requires_injection() {
    let fx = fixture(Duration::from_secs(30));
    let (approval, _) = fx.broker.open(
        dal_core::Owner::Core,
        Question::Approval {
            tool: "run".into(),
            preview: dal_core::Preview {
                title: "run".into(),
                body: "git status".into(),
                digest: None,
            },
            grant: None,
            call: None,
        },
        turn(),
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    let (question, _) = fx.broker.open(
        dal_core::Owner::Core,
        Question::Confirm {
            text: "continue?".into(),
        },
        turn(),
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    assert_ne!(approval.id, question.id);

    let missing = caller("focus", &[], None);
    assert!(matches!(
        fx.services.open_asks(&missing).await,
        Err(ServiceError::Denied(DenyReason::NotInjected))
    ));
    let allowed = caller("focus", &["ask"], None);
    assert_eq!(fx.services.open_asks(&allowed).await.expect("ask count"), 1);
    assert!(
        fx.services
            .scheme(&missing, "todo://terminal")
            .await
            .expect("read-only scheme")
            .is_none()
    );
}

#[tokio::test]
async fn services_use_one_shared_capability_gate() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller(
        "focus",
        &["agents", "jobs", "turn", "sidecar"],
        Some(turn()),
    );
    *fx.backend.agents_reply.lock().unwrap() = Some(AgentsReply::Started {
        id: SessionId::new_v7(),
    });
    *fx.backend.jobs_reply.lock().unwrap() = Some(JobsReply::Spawned {
        id: JobId::new_v7(),
    });
    *fx.backend.turn_reply.lock().unwrap() = Some(TurnOpReply::Idle(true));
    *fx.backend.sidecar_value.lock().unwrap() = Some(b"value".to_vec());

    let services = Arc::clone(&fx.services);
    let denied = spawn(async move { services.agents(&who, AgentsOp::List).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(matches!(denied.join().await, Err(ServiceError::Declined)));

    let services = Arc::clone(&fx.services);
    let who = caller(
        "focus",
        &["agents", "jobs", "turn", "sidecar"],
        Some(turn()),
    );
    let granted = spawn(async move { services.agents(&who, AgentsOp::List).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    assert!(matches!(
        granted.join().await,
        Ok(AgentsReply::Started { .. })
    ));

    // One key covers the whole inject set: the rest pass with no new ask.
    let who = caller(
        "focus",
        &["agents", "jobs", "turn", "sidecar"],
        Some(turn()),
    );
    let spawned = fx
        .services
        .jobs(
            &who,
            JobsOp::Spawn {
                name: "nightly".parse().unwrap(),
                payload: RawJson::parse("{}").unwrap(),
                parent: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(spawned, JobsReply::Spawned { .. }));
    let idle = fx
        .services
        .turn(&who, TurnOp::IsIdle)
        .await
        .expect("turn shares the grant");
    assert_eq!(idle, TurnOpReply::Idle(true));
    let value = fx
        .services
        .sidecar(
            &who,
            SidecarOp::Read {
                name: "slot".parse().unwrap(),
            },
        )
        .await
        .expect("sidecar shares the grant");
    assert_eq!(value, Some(b"value".to_vec()));
    assert_eq!(
        open_count(&fx.broker),
        0,
        "one approval covers every service"
    );

    let ghost = ephemeral_fixture(Duration::from_secs(30));
    let who = caller("focus", &["sidecar"], Some(turn()));
    let services = Arc::clone(&ghost.services);
    let ephemeral = spawn(async move {
        services
            .sidecar(
                &who,
                SidecarOp::Read {
                    name: "slot".parse().unwrap(),
                },
            )
            .await
    });
    await_open(&ghost.broker).await;
    answer_next(&ghost.broker, Answer::Approve);
    match ephemeral.join().await {
        Err(ServiceError::Denied(DenyReason::Unavailable { what })) => {
            assert_eq!(&*what, "sidecar (ephemeral session)");
            assert_eq!(
                super::SIDECAR_EPHEMERAL_TEXT,
                "sidecar is unavailable for ephemeral sessions",
            );
        }
        Err(other) => panic!("the denial names the wrong resource: {other:?}"),
        Ok(_) => panic!("expected an unavailable sidecar"),
    }
}

fn sidecar_read() -> SidecarOp {
    SidecarOp::Read {
        name: "slot".parse().unwrap(),
    }
}

#[tokio::test]
async fn a_command_caller_without_a_turn_is_asked_for_the_grant_it_lacks() {
    let fx = fixture(Duration::from_secs(30));
    *fx.backend.sidecar_value.lock().unwrap() = Some(b"value".to_vec());
    let who = caller("focus", &["sidecar"], None);

    let services = Arc::clone(&fx.services);
    let first = {
        let who = who.clone();
        spawn(async move { services.sidecar(&who, sidecar_read()).await })
    };
    await_open(&fx.broker).await;
    let open = fx.broker.open_requests();
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0].turn, None,
        "a command question has no turn to end with"
    );
    assert!(
        matches!(&open[0].question, Question::Grant { ext, capabilities, .. }
            if &**ext == "focus" && capabilities.iter().map(|c| &**c).eq(["sidecar"])),
        "the question names the plugin and its declared services: {:?}",
        open[0].question
    );
    answer_next(&fx.broker, Answer::Approve);
    assert_eq!(first.join().await.unwrap(), Some(b"value".to_vec()));

    let again = fx.services.sidecar(&who, sidecar_read()).await.unwrap();
    assert_eq!(again, Some(b"value".to_vec()));
    assert_eq!(
        open_count(&fx.broker),
        0,
        "the approval is stored: no second ask"
    );
}

#[tokio::test]
async fn a_declined_command_grant_denies_and_asks_again_next_time() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &["sidecar"], None);

    let services = Arc::clone(&fx.services);
    let declined = {
        let who = who.clone();
        spawn(async move { services.sidecar(&who, sidecar_read()).await })
    };
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(matches!(declined.join().await, Err(ServiceError::Declined)));

    let services = Arc::clone(&fx.services);
    let retried = spawn(async move { services.sidecar(&who, sidecar_read()).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(matches!(retried.join().await, Err(ServiceError::Declined)));
}

#[tokio::test]
async fn a_grant_question_with_no_answering_front_end_is_denied_at_once() {
    for turn in [None, Some(turn())] {
        let fx = fixture(Duration::from_secs(30));
        fx.backend
            .headless
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let who = caller("focus", &["sidecar"], turn);
        let denied = tokio::time::timeout(
            Duration::from_secs(5),
            fx.services.sidecar(&who, sidecar_read()),
        )
        .await
        .expect("a headless grant question never waits out its timeout");
        assert!(
            matches!(
                &denied,
                Err(ServiceError::Denied(DenyReason::ServiceNotGranted { service, plugin }))
                    if *service == Service::Sidecar && &**plugin == "focus"
            ),
            "{denied:?}"
        );
        assert_eq!(open_count(&fx.broker), 0, "no request opens for nobody");

        fx.backend
            .headless
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *fx.backend.sidecar_value.lock().unwrap() = Some(b"value".to_vec());
        let services = Arc::clone(&fx.services);
        let asked = spawn(async move { services.sidecar(&who, sidecar_read()).await });
        await_open(&fx.broker).await;
        answer_next(&fx.broker, Answer::Approve);
        assert_eq!(
            asked.join().await.unwrap(),
            Some(b"value".to_vec()),
            "the denial does not strand the key: a front end attached later is asked"
        );
    }
}

#[tokio::test]
async fn sidecar_rejects_values_over_limit() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &["sidecar"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let operation = spawn(async move {
        services
            .sidecar(
                &who,
                SidecarOp::Write {
                    name: "large".parse().unwrap(),
                    bytes: vec![0; 1_048_577],
                },
            )
            .await
    });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    let error = operation
        .join()
        .await
        .expect_err("oversized sidecar refused");
    assert_eq!(
        error.to_string(),
        ServiceError::sidecar_too_large("large", 1_048_577).to_string()
    );
}

#[tokio::test]
async fn rust_infer_is_trusted_and_script_infer_is_gated() {
    let fx = fixture(Duration::from_secs(30));
    *fx.backend.infer_reply.lock().unwrap() = Some(Inference { events: Vec::new() });

    let bare = caller("focus", &[], Some(turn()));
    let inference = fx
        .services
        .infer(&bare, model_request())
        .await
        .expect("rust infer is trusted");
    assert_eq!(inference.events, []);
    assert_eq!(open_count(&fx.broker), 0, "trusted infer asks nothing");

    let denied = fx
        .services
        .script_infer(&bare, model_request())
        .await
        .unwrap_err();
    assert!(matches!(
        denied,
        ServiceError::Denied(DenyReason::NotInjected)
    ));

    let gated = caller("focus", &["infer"], Some(turn()));
    let cancelled = fixture(Duration::from_secs(30));
    *cancelled.backend.infer_reply.lock().unwrap() = Some(Inference { events: Vec::new() });
    cancelled.cancel.cancel();
    let error = cancelled
        .services
        .script_infer(&gated, model_request())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Denied(DenyReason::ServiceNotGranted { .. })
    ));

    let services = Arc::clone(&fx.services);
    let approved = spawn(async move { services.script_infer(&gated, model_request()).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    let inference = approved.join().await.expect("approved script infer runs");
    assert_eq!(inference.events, []);
}

#[tokio::test]
async fn scripted_forward_requires_the_callers_exact_infer_grant() {
    let fx = fixture(Duration::from_secs(30));
    let cancel = CancellationToken::new();
    let missing = caller("focus", &[], Some(turn()));
    assert!(matches!(
        fx.services
            .authorize_script_infer(&missing, cancel.clone())
            .await,
        Err(ServiceError::Denied(DenyReason::NotInjected))
    ));
    assert_eq!(open_count(&fx.broker), 0, "no grant read without injection");

    let authorized = caller("focus", &["infer"], Some(turn()));
    fx.backend
        .headless
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        fx.services
            .authorize_script_infer(&authorized, cancel.clone())
            .await,
        Err(ServiceError::Denied(DenyReason::ServiceNotGranted {
            service: Service::Infer,
            ..
        }))
    ));
    assert_eq!(open_count(&fx.broker), 0, "headless access fails closed");

    fx.backend
        .headless
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let services = Arc::clone(&fx.services);
    let who = authorized.clone();
    let approved = spawn(async move {
        services
            .authorize_script_infer(&who, CancellationToken::new())
            .await
    });
    await_open(&fx.broker).await;
    let open = fx.broker.open_requests();
    assert!(
        matches!(&open[0].question, Question::Grant { ext, capabilities, .. }
            if &**ext == "focus" && capabilities.iter().map(|c| &**c).eq(["infer"])),
        "the grant must name the exact extension and service: {:?}",
        open[0].question
    );
    answer_next(&fx.broker, Answer::ApproveForSession);
    approved
        .join()
        .await
        .expect("the live controller approved Infer");
    fx.services
        .authorize_script_infer(&authorized, cancel.clone())
        .await
        .expect("the exact grant is reusable");
    assert_eq!(open_count(&fx.broker), 0, "reuse asks nothing");

    fx.backend
        .headless
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let different_origin = Caller::new(
        Name::parse("focus").expect("name"),
        Origin::Bundled,
        ServiceSet::from_names(["infer"]).expect("inject"),
        std::num::NonZeroU32::MIN,
        CallerKind::Handler,
        Some(turn()),
    );
    let expanded_services = caller("focus", &["infer", "net"], Some(turn()));
    for changed in [&different_origin, &expanded_services] {
        assert!(
            matches!(
                fx.services
                    .authorize_script_infer(changed, cancel.clone())
                    .await,
                Err(ServiceError::Denied(DenyReason::ServiceNotGranted {
                    service: Service::Infer,
                    ..
                }))
            ),
            "an origin or service-set change must invalidate grant reuse"
        );
    }
    assert_eq!(
        open_count(&fx.broker),
        0,
        "changed identities remain denied"
    );

    // A session grant is scoped to its session's store, not just its plugin.
    let second_session = fixture(Duration::from_secs(30));
    second_session
        .backend
        .headless
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(
        second_session
            .services
            .authorize_script_infer(&authorized, CancellationToken::new())
            .await,
        Err(ServiceError::Denied(DenyReason::ServiceNotGranted {
            service: Service::Infer,
            ..
        }))
    ));
    assert_eq!(open_count(&second_session.broker), 0);
}

fn run_request(argv: &[&str], cwd: PathBuf) -> dal_core::RunRequest {
    dal_core::RunRequest {
        argv: argv.iter().map(OsString::from).collect(),
        cwd: Some(cwd),
        stdin: None,
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    }
}

#[tokio::test]
async fn run_checks_call_grant_then_exec_ladder() {
    let fx = fixture(Duration::from_secs(30));
    let workspace = fx.temp.path().to_path_buf();
    let git = vec![OsString::from("git"), OsString::from("status")];
    fx.rt.script.lock().unwrap().extend([
        AuthorizeScript::AskThenApprove {
            prefix: vec![OsString::from("git")],
            roots: vec![workspace.clone()],
        },
        AuthorizeScript::Covered {
            prefix: vec![OsString::from("git")],
            roots: vec![workspace.clone()],
        },
        AuthorizeScript::AskThenApprove {
            prefix: vec![OsString::from("ls")],
            roots: vec![workspace.clone()],
        },
        AuthorizeScript::Deny(DenyReason::OutOfScope {
            what: "the working directory is outside the approved roots".into(),
        }),
        AuthorizeScript::AskThenApprove {
            prefix: vec![OsString::from("git")],
            roots: vec![workspace.clone()],
        },
    ]);
    let who = caller("focus", &["run"], Some(turn()));

    // First call: the capability grant asks once, then the ladder approves.
    let services = Arc::clone(&fx.services);
    let mut request = run_request(&["git", "status"], workspace.clone());
    request.env = vec![("DAL_TEST_OVERRIDE".into(), "visible".into())];
    let first = spawn(async move { services.run(&who, request).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    let error = first.join().await.unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Failed {
            service: Some(Service::Run),
            ..
        }
    ));
    assert_eq!(
        fx.rt.previews.lock().unwrap().len(),
        1,
        "the exec approval request"
    );
    let preview = fx.rt.previews.lock().unwrap().pop().unwrap();
    let title: &str = &preview.title;
    let body: &str = &preview.body;
    assert!(title.contains("git"), "preview names the tool");
    assert!(body.contains("git status"), "preview shows the command");
    assert_eq!(*fx.rt.ladder_asks.lock().unwrap(), 1);
    {
        let spawns = fx.rt.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].argv, git);
        assert_eq!(spawns[0].cwd, workspace);
        assert_eq!(
            spawns[0].env,
            vec![(
                OsString::from("DAL_TEST_OVERRIDE"),
                OsString::from("visible")
            )]
        );
    }

    // Same prefix while its grant lives: the ladder asks nothing new.
    let who = caller("focus", &["run"], Some(turn()));
    let request = run_request(&["git", "status"], workspace.clone());
    let error = fx.services.run(&who, request).await.unwrap_err();
    assert!(matches!(error, ServiceError::Failed { .. }));
    assert_eq!(*fx.rt.ladder_asks.lock().unwrap(), 1, "no second ask");
    assert_eq!(fx.rt.spawns.lock().unwrap().len(), 2);
    assert_eq!(open_count(&fx.broker), 0, "the capability grant persists");

    // A new prefix asks again through the ladder.
    let request = run_request(&["ls"], workspace.clone());
    let error = fx.services.run(&who, request).await.unwrap_err();
    assert!(matches!(error, ServiceError::Failed { .. }));
    assert_eq!(*fx.rt.ladder_asks.lock().unwrap(), 2);
    assert_eq!(fx.rt.spawns.lock().unwrap().len(), 3);

    // Outside the approved roots the ladder denies and nothing spawns.
    let outside = fx.temp.path().join("elsewhere");
    let request = run_request(&["git", "status"], outside);
    let error = fx.services.run(&who, request).await.unwrap_err();
    assert!(matches!(
        error,
        ServiceError::Denied(DenyReason::OutOfScope { .. })
    ));
    assert_eq!(
        fx.rt.spawns.lock().unwrap().len(),
        3,
        "denied runs never spawn"
    );

    // After the job ends the ladder asks again for the same prefix.
    let request = run_request(&["git", "status"], workspace.clone());
    let error = fx.services.run(&who, request).await.unwrap_err();
    assert!(matches!(error, ServiceError::Failed { .. }));
    assert_eq!(*fx.rt.ladder_asks.lock().unwrap(), 3);
    assert_eq!(fx.rt.spawns.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn run_hands_the_requested_stdout_prefix_limit_to_the_spawn() {
    let fx = fixture(Duration::from_secs(30));
    let workspace = fx.temp.path().to_path_buf();
    fx.rt
        .script
        .lock()
        .unwrap()
        .push_back(AuthorizeScript::AskThenApprove {
            prefix: vec![OsString::from("git")],
            roots: vec![workspace.clone()],
        });
    let who = caller("focus", &["run"], Some(turn()));
    let mut request = run_request(&["git", "status"], workspace);
    request.stdout_prefix_limit = 4096;
    let services = Arc::clone(&fx.services);
    let call = spawn(async move { services.run(&who, request).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    let error = call.join().await.unwrap_err();
    assert!(matches!(error, ServiceError::Failed { .. }));
    let spawns = fx.rt.spawns.lock().unwrap();
    assert_eq!(spawns.len(), 1);
    assert_eq!(
        spawns[0].stdout_prefix_limit, 4096,
        "the child keeps the stdout prefix the caller asked for"
    );
}

#[tokio::test(start_paused = true)]
async fn ask_queue_preserves_answers_and_records_deadline_unavailable() {
    let fx = fixture(Duration::from_millis(50));
    let ask = |services: Arc<SessionServices>, turn: TurnId| async move {
        let who = caller("focus", &["ask"], Some(turn));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    };

    // An answered question resolves to its value.
    let first = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    answer_next(
        &fx.broker,
        Answer::Value(RawJson::parse("\"yes\"").unwrap()),
    );
    assert!(first.join().await.expect("an answer resolves").is_some());

    // A dismissal resolves to no answer.
    let second = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(second.join().await.expect("a dismissal resolves").is_none());

    // A client Cancel is an empty submission: a dismiss, never an interrupt.
    let dismissed = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Cancel);
    assert!(
        dismissed
            .join()
            .await
            .expect("a dismissal resolves")
            .is_none()
    );

    // A turn cancellation interrupts the open question. A bare client
    // Cancel is a dismissal instead, covered above.
    let third = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    cancel_next(&fx.broker, turn());
    assert!(matches!(third.join().await, Err(ServiceError::Cancelled)));

    // A local timeout records an unavailable deadline, not a cancellation.
    let timed_out = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    tokio::time::advance(Duration::from_millis(50)).await;
    assert!(
        timed_out
            .join()
            .await
            .expect("a timeout resolves")
            .is_none()
    );
    let claim = fx
        .inbox
        .take()
        .expect("the local deadline routes one claim");
    assert!(
        matches!(
            claim,
            crate::broker::Resolved {
                resolution: Resolution::Unavailable,
                was_default: true,
                answer: Answer::Cancel,
                cause: ResolutionCause::TimedOut,
                ..
            }
        ),
        "the local deadline must journal one attended unavailable default: {claim:?}"
    );
    fx.inbox.mark_settled(claim.request.id);
    assert!(
        fx.broker.open_requests().is_empty(),
        "the timed-out request is resolved in the broker"
    );

    // A second simultaneous ask queues instead of failing: both requests
    // are open at once, the settled concurrency rule, and each resolves on
    // its own answer.
    let pending = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    let queued = spawn(ask(Arc::clone(&fx.services), turn()));
    await_second_open(&fx.broker).await;
    assert_eq!(fx.broker.open_requests().len(), 2);
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        pending
            .join()
            .await
            .expect("the first ask resolves")
            .is_none()
    );
    answer_next(
        &fx.broker,
        Answer::Value(RawJson::parse("\"late\"").unwrap()),
    );
    assert_eq!(
        queued.join().await.expect("the queued ask resolves"),
        Some(Answer::Value(RawJson::parse("\"late\"").unwrap()))
    );

    // A turn cancellation interrupts the open question.
    let cancelled_fx = fixture(Duration::from_secs(30));
    let waiting = spawn(ask(Arc::clone(&cancelled_fx.services), turn()));
    await_open(&cancelled_fx.broker).await;
    cancelled_fx.cancel.cancel();
    assert!(matches!(waiting.join().await, Err(ServiceError::Cancelled)));
}

#[tokio::test]
async fn ask_requests_carry_the_configured_timeout() {
    let fx = fixture(Duration::from_secs(60));
    let services = Arc::clone(&fx.services);
    let waiting = spawn(async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    });
    await_open(&fx.broker).await;
    let request = fx
        .broker
        .open_requests()
        .into_iter()
        .next()
        .expect("an open request");
    assert!(
        request.timeout >= Duration::from_secs(59) && request.timeout <= Duration::from_secs(60),
        "the broker deadline comes from the configured timeout: {:?}",
        request.timeout
    );
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        waiting
            .join()
            .await
            .expect("a dismissal resolves")
            .is_none()
    );
}

#[tokio::test]
async fn an_open_ask_survives_a_generation_swap() {
    let fx = fixture(Duration::from_secs(30));
    let services = Arc::clone(&fx.services);
    let waiting = spawn(async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    });
    await_open(&fx.broker).await;
    fx.generation.send_replace(generation_of(Vec::new()));
    answer_next(
        &fx.broker,
        Answer::Value(RawJson::parse("\"yes\"").unwrap()),
    );
    assert_eq!(
        waiting.join().await.expect("the answer resolves"),
        Some(Answer::Value(RawJson::parse("\"yes\"").unwrap()))
    );
}

/// Builds the unavailable-client request; the field shape follows the core
/// extension values part and collapses to one place if it lands wider.
fn mcp_request() -> dal_core::ext::McpRequest {
    dal_core::ext::McpRequest {
        session: SessionId::new_v7(),
        server: "files".into(),
        tool: "read".into(),
        arguments: RawJson::parse("{}").unwrap(),
    }
}

#[tokio::test]
async fn mcp_unavailable_has_fixed_text() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &["mcp"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let missing = spawn(async move { services.mcp(&who, mcp_request()).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    match missing.join().await {
        Err(ServiceError::Denied(DenyReason::Unavailable { what })) => {
            // The denial names the missing resource. The user-facing guidance
            // sentence (`MCP_UNAVAILABLE_TEXT`) is owned by the product contract
            // and deliberately not byte-pinned here, so rewording stays a doc edit.
            assert_eq!(&*what, "MCP client");
        }
        Err(other) => panic!("the denial names the wrong resource: {other:?}"),
        Ok(_) => panic!("expected an unavailable MCP client"),
    }
}

fn skill_declaring(name: &str, mcp: Option<McpBlock>) -> SkillRecord {
    SkillRecord {
        name: name.parse::<Name>().unwrap(),
        description: "d".into(),
        body: "body".into(),
        letter2image: false,
        mcp,
    }
}

fn block_of(server: &str, decl: McpServerDecl) -> McpBlock {
    McpBlock {
        servers: [(Box::<str>::from(server), decl)].into(),
    }
}

fn http_decl(url: &str) -> McpServerDecl {
    McpServerDecl::Http {
        endpoint: McpEndpoint::parse(url).expect("fixture endpoint"),
    }
}

fn plugin_with(name: &str, skills: Vec<SkillRecord>) -> Extension {
    let mcp = ServiceSet::from_names(["mcp"]).unwrap();
    let mut builder = ExtensionBuilder::new(name, "1.0.0", mcp).unwrap();
    for skill in skills {
        builder = builder.skill(skill);
    }
    builder.build().unwrap()
}

#[tokio::test]
async fn mcp_declarations_list_declared_blocks_and_follow_reload() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("battery", &[], None);
    assert_eq!(fx.services.mcp_declarations(&who).await.unwrap(), vec![]);

    let web = block_of("web", http_decl("https://docs.example/mcp"));
    let files = block_of(
        "files",
        McpServerDecl::Stdio {
            launch: McpLaunch::command(vec!["npx".into(), "files-server".into()])
                .expect("fixture launch"),
            env: [("TOKEN".into(), "literal".into())].into(),
        },
    );
    fx.generation.send_replace(generation_of(vec![
        plugin_with(
            "beta",
            vec![
                skill_declaring("docs", Some(web.clone())),
                skill_declaring("plain", None),
            ],
        ),
        plugin_with("alpha", vec![skill_declaring("code", Some(files.clone()))]),
    ]));
    let declared = |plugin: &str, skill: &str, block: McpBlock| McpDeclaration {
        plugin: plugin.parse().unwrap(),
        skill: skill.parse().unwrap(),
        block,
    };
    assert_eq!(
        fx.services.mcp_declarations(&who).await.unwrap(),
        vec![
            declared("beta", "docs", web),
            declared("alpha", "code", files),
        ],
        "one entry per declaring skill in generation extension order; undeclared skills absent"
    );

    fx.generation.send_replace(generation_of(vec![plugin_with(
        "beta",
        vec![skill_declaring("plain", None)],
    )]));
    assert_eq!(fx.services.mcp_declarations(&who).await.unwrap(), vec![]);
}

#[tokio::test]
async fn env_reads_only_the_requested_key() {
    let fx = fixture(Duration::from_secs(30));
    *fx.backend.env_value.lock().unwrap() = Some(String::from("sekret"));
    let who = caller("focus", &["env"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let reading = spawn(async move { services.env(&who, "DAL_KEY").await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    assert_eq!(
        reading.join().await.expect("one key reads").as_deref(),
        Some("sekret")
    );
    assert_eq!(
        *fx.backend.seen_env.lock().unwrap(),
        vec![String::from("DAL_KEY")],
        "no enumeration operation exists"
    );
}

#[test]
fn run_output_maps_only_truthful_fields() {
    let result = crate::proc::ProcResult {
        status: crate::proc::ProcStatus::Exited { code: 0 },
        outcome: dal_core::JobOutcome::Exited { code: 0 },
        preview: b"tail".to_vec().into_boxed_slice(),
        log_path: PathBuf::from("/tmp/x.log"),
        stdout_prefix: b"pre".to_vec().into_boxed_slice(),
        stdout_prefix_overflowed: true,
        denial_seen: false,
        completion_tail: b"done".to_vec().into_boxed_slice(),
    };
    let output = super::run_output_of(&result);
    assert_eq!(output.status, dal_core::ExitStatusKind::Exited(0));
    assert_eq!(output.stdout_tail, b"tail".to_vec());
    assert_eq!(output.stdout_prefix, b"pre".to_vec());
    assert!(output.stdout_prefix_overflowed);
    assert!(
        output.stderr_tail.is_empty(),
        "no separate stderr tail exists"
    );
    assert_eq!(output.log, Some(PathBuf::from("/tmp/x.log")));
}

#[tokio::test]
async fn records_keep_only_the_caller_kind() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &[], None);
    let other = caller("other", &[], None);
    let body = || Box::new(RawJson::parse("{\"n\":1}").unwrap());
    fx.services
        .append_record(&who, "letter", body())
        .await
        .unwrap();
    fx.services
        .append_record(&who, "letter", body())
        .await
        .unwrap();
    fx.services
        .append_record(&who, "dream", body())
        .await
        .unwrap();
    fx.services
        .append_record(&other, "letter", body())
        .await
        .unwrap();
    let letters = fx.services.records(&who, "letter").await.unwrap();
    assert_eq!(letters.len(), 2, "only the caller's own kind returns");
    assert!(letters.iter().all(|record| record.as_str() == "{\"n\":1}"));
    assert_eq!(fx.services.records(&who, "missing").await.unwrap(), []);
}

#[tokio::test]
async fn scope_over_uses_the_supplied_service_caller() {
    let fx = fixture(Duration::from_secs(30));
    let who = caller("focus", &[], None);
    let services: Arc<dyn Services> = fx.services.clone();
    let scope = Scope::over(
        services,
        &who,
        fx.cancel.clone(),
        ScopeSpec {
            limit: 1,
            on_error: OnError::Settle,
            budget: Budget::default(),
        },
    )
    .expect("scope opens");
    let handle = scope
        .agent(dal_core::AgentStart {
            call: CallId::new("member"),
            name: Some("member".into()),
            prompt: "work".into(),
            model: None,
            role: None,
            system: None,
            tools: None,
            workspace: None,
        })
        .expect("handle admitted");
    assert!(matches!(
        handle.result().await,
        Err(ScopeError::Denied(DenyReason::NotInjected))
    ));
}

#[tokio::test]
async fn a_dropped_ask_resolves_its_broker_request() {
    let fx = fixture(Duration::from_secs(60));
    let who = caller("focus", &["ask"], Some(turn()));
    let mut ask = Box::pin(fx.services.ask(
        &who,
        Question::Text {
            prompt: "why?".into(),
            placeholder: None,
        },
    ));
    assert!(
        futures::poll!(ask.as_mut()).is_pending(),
        "the ask waits for an answer"
    );
    drop(ask);
    assert!(
        fx.broker.open_requests().is_empty(),
        "a dropped ask retires its broker request"
    );
    let updates = fx.backend.updates.lock().unwrap();
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, dal_core::UpdateKind::RequestOpened(_))),
        "the question was published: {updates:?}"
    );
    drop(updates);
    let claim = fx
        .inbox
        .take()
        .expect("the dropped guard routes one withdrawal claim");
    assert_eq!(claim.answer, dal_core::Answer::Cancel);
    assert_eq!(claim.cause, ResolutionCause::Withdrawn);
}

#[tokio::test]
async fn an_ask_with_no_answerer_attached_defaults_at_once() {
    let fx = fixture(Duration::from_secs(60));
    fx.backend
        .headless
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let who = caller("focus", &["ask"], Some(turn()));
    let answer = tokio::time::timeout(
        Duration::from_secs(1),
        fx.services.ask(
            &who,
            Question::Text {
                prompt: "why?".into(),
                placeholder: None,
            },
        ),
    )
    .await
    .expect("a headless ask never waits for its timeout")
    .expect("a headless ask is a default, not an error");
    assert_eq!(answer, None, "the fail-closed default is no answer");
    assert!(
        fx.broker.open_requests().is_empty(),
        "no request opens for a front end nobody runs"
    );
    assert!(fx.backend.updates.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_open_ask_times_out_at_its_absolute_deadline() {
    let fx = fixture(Duration::from_secs(60));
    let services = Arc::clone(&fx.services);
    let who = caller("focus", &["ask"], Some(turn()));
    let mut asked = JoinSet::new();
    asked.spawn(async move {
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "why?".into(),
                    placeholder: None,
                },
            )
            .await
    });
    await_open(&fx.broker).await;

    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(
        asked.try_join_next().is_none(),
        "the question stays open until its absolute deadline"
    );
    assert_eq!(fx.broker.open_requests().len(), 1);

    tokio::time::advance(Duration::from_secs(1)).await;
    let outcome = asked
        .join_next()
        .await
        .expect("the ask settles")
        .expect("the ask task joins");
    assert!(
        matches!(outcome, Ok(None)),
        "the absolute timeout resolves to no answer: {outcome:?}"
    );
}

/// F3: an answerer that detaches mid-wait shortens the pending ask instead
/// of leaving it to wait out its full timeout. Fails before the detachment
/// watcher: the ask sleeps the whole 60s and the 10s join times out.
#[tokio::test]
async fn an_ask_shortens_when_the_answerer_detaches_mid_wait() {
    let fx = fixture(Duration::from_secs(60));
    let services = Arc::clone(&fx.services);
    let waiting = spawn(async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "why?".into(),
                    placeholder: None,
                },
            )
            .await
    });
    await_open(&fx.broker).await;
    fx.backend
        .headless
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiting.join())
        .await
        .expect("a detached ask never waits out its full timeout");
    assert!(
        matches!(outcome, Ok(None)),
        "a detached ask fails closed like the headless admission: {outcome:?}"
    );
    assert!(
        fx.inbox.take().is_none(),
        "no withdrawal claim: the actor's expiry sweep still owns the cause"
    );
    assert_eq!(
        fx.broker.open_requests().len(),
        1,
        "the broker slot stays open for the sweep; only the waiter shortens"
    );
}

/// F2: an answer journaled and released before the local sleep fires wins
/// over the synthesized timeout default when both are ready at the waiter's
/// next poll. The future is polled by hand so no scheduling can resolve the
/// race before the assertion: a spawned waiter may observe the answer first
/// and pass even without the repair. Fails before the answer-first repair:
/// the biased sleep arm reports `Ok(None)`.
#[tokio::test(start_paused = true)]
async fn an_answer_released_before_the_local_timeout_wins_over_it() {
    let fx = fixture(Duration::from_secs(60));
    let who = caller("focus", &["ask"], Some(turn()));
    let mut ask = Box::pin(fx.services.ask(
        &who,
        Question::Text {
            prompt: "why?".into(),
            placeholder: None,
        },
    ));
    assert!(
        futures::poll!(ask.as_mut()).is_pending(),
        "the ask waits for an answer"
    );
    answer_next(
        &fx.broker,
        Answer::Value(RawJson::parse("\"yes\"").unwrap()),
    );
    tokio::time::advance(Duration::from_secs(60)).await;
    match futures::poll!(ask.as_mut()) {
        std::task::Poll::Ready(Ok(Some(Answer::Value(body)))) => assert_eq!(
            body,
            RawJson::parse("\"yes\"").unwrap(),
            "the journaled answer wins over the timeout"
        ),
        other => panic!("the journaled answer wins over the timeout: {other:?}"),
    }
}
/// A fired turn token cancels turn-scoped service calls without touching
/// the session token.
#[tokio::test]
async fn a_fired_turn_token_cancels_turn_scoped_calls() {
    let fx = fixture(Duration::from_secs(30));
    fx.turn_token.cancel();
    let run = fx
        .services
        .run(
            &caller("focus", &["run"], Some(turn())),
            run_request(&["echo"], fx.temp.path().to_path_buf()),
        )
        .await;
    assert!(
        matches!(run, Err(ServiceError::Cancelled)),
        "a dead turn must not run: {run:?}"
    );
    let ask = fx
        .services
        .ask(
            &caller("focus", &["ask"], Some(turn())),
            Question::Text {
                prompt: "proceed?".into(),
                placeholder: None,
            },
        )
        .await;
    assert!(
        matches!(ask, Err(ServiceError::Cancelled)),
        "a dead turn must not ask: {ask:?}"
    );
}

/// The reserved child-policy identity is denied at the extension door, on
/// any session, while ordinary records still pass.
#[tokio::test]
async fn the_child_policy_identity_is_denied_to_extensions() {
    let fx = fixture(Duration::from_secs(30));
    let body = || Box::new(RawJson::parse("{}").unwrap());
    let forged = fx
        .services
        .append_record(
            &caller("dal-agent", &["sidecar"], None),
            "child_policy",
            body(),
        )
        .await;
    assert!(
        matches!(forged, Err(ServiceError::Denied(_))),
        "a forged child policy must be denied: {forged:?}"
    );
    assert!(
        fx.services
            .append_record(&caller("dal-agent", &["sidecar"], None), "notes", body())
            .await
            .is_ok(),
        "an ordinary dal-agent record must pass"
    );
    assert!(
        fx.services
            .append_record(&caller("focus", &["sidecar"], None), "child_policy", body())
            .await
            .is_ok(),
        "another extension's own kind must pass"
    );
}

/// A withdrawn ask slot keeps its admission occupied until the actor
/// settles the claim: the capacity the dropped slot held is not reusable
/// before `mark_settled`, which is what bounds the withdrawal queue.
#[tokio::test]
async fn a_withdrawn_ask_slot_stays_occupied_until_settlement() {
    use super::MAX_OPEN_ASKS;

    let fx = fixture(Duration::from_secs(60));
    let ask = |services: Arc<SessionServices>| async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    };
    let withdrawn = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    let mut live = Vec::new();
    for _ in 1..MAX_OPEN_ASKS {
        live.push(spawn(ask(Arc::clone(&fx.services))));
    }
    for _ in 0..100 {
        if open_count(&fx.broker) >= MAX_OPEN_ASKS {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        open_count(&fx.broker),
        MAX_OPEN_ASKS,
        "the ask battery fills to its cap"
    );

    // Aborting the task drops its armed guard, which claims a withdrawal;
    // the claim is unsettled, so the slot's capacity stays occupied.
    drop(withdrawn);
    let claim = loop {
        if let Some(claim) = fx.inbox.take() {
            break claim;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(claim.cause, ResolutionCause::Withdrawn, "{claim:?}");
    let busy = fx
        .services
        .ask(
            &caller("focus", &["ask"], Some(turn())),
            Question::Text {
                prompt: "one more?".into(),
                placeholder: None,
            },
        )
        .await;
    assert!(
        matches!(busy, Err(ServiceError::Failed { .. })),
        "an unsettled withdrawal blocks admission: {busy:?}"
    );

    // The actor's durable settlement frees the slot and the next ask opens.
    fx.inbox.mark_settled(claim.request.id);
    let reopened = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        reopened
            .join()
            .await
            .expect("the reopened ask resolves")
            .is_none(),
        "settling the claim reopens the capacity"
    );
    drop(live);
}

/// A lost withdrawal race never wedges the battery: when another winner
/// resolved the slot first, the armed drop's claim comes back `None` and
/// the slot's admission frees at once, without any settlement.
#[tokio::test]
async fn a_raced_withdrawal_frees_the_ask_slot_without_settlement() {
    let fx = fixture(Duration::from_secs(60));
    let ask = |services: Arc<SessionServices>| async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    };
    let first = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;

    // The actor's sweep wins the CAS: the request expires, journals its
    // default, and releases the waiter before the dropped guard claims.
    let id = fx
        .broker
        .open_requests()
        .into_iter()
        .next()
        .expect("the ask opened")
        .id;
    let expired = fx
        .broker
        .expire_one(
            id,
            tokio::time::Instant::now() + Duration::from_secs(61),
            false,
        )
        .expect("the sweep claims first");
    fx.broker.release(&expired);
    fx.inbox.mark_settled(id);
    assert!(matches!(first.join().await, Ok(None)), "{expired:?}");

    // The dropped guard races the already-resolved slot: no claim queues,
    // the receipt is None, and the freed entry admits the next ask without
    // any further settlement.
    let claim = fx.inbox.take();
    assert!(claim.is_none(), "no claim raced through: {claim:?}");
    let second = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        second
            .join()
            .await
            .expect("the next ask opens after a lost race")
            .is_none()
    );
}

/// A run approval whose slot was settled by the actor's sweep frees its
/// exclusive admission on the guard's lost-race drop: a later run can
/// approve again instead of failing busy forever.
#[tokio::test]
async fn a_raced_run_withdrawal_frees_the_run_slot_without_settlement() {
    let fx = fixture(Duration::from_secs(30));
    let workspace = fx.temp.path().to_path_buf();
    {
        // Both runs take the broker approval path: without a queued `Ask`
        // the ladder allows at once and no run approval ever opens.
        fx.rt.decisions.lock().unwrap().extend([
            dal_core::Decision::Ask { grant: None },
            dal_core::Decision::Ask { grant: None },
        ]);
    }
    let who = caller("focus", &["run"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let first = spawn(async move {
        services
            .run(&who, run_request(&["git", "status"], workspace))
            .await
    });
    // The capability grant asks first and persists: approve it so the run
    // reaches its own approval, the slot under test.
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    await_open(&fx.broker).await;

    let id = fx
        .broker
        .open_requests()
        .into_iter()
        .next()
        .expect("the run approval opened")
        .id;
    let expired = fx
        .broker
        .expire_one(
            id,
            tokio::time::Instant::now() + Duration::from_secs(301),
            false,
        )
        .expect("the sweep settles the approval first");
    fx.broker.release(&expired);
    fx.inbox.mark_settled(id);
    let error = first.join().await.unwrap_err();
    assert!(
        matches!(error, ServiceError::Denied(DenyReason::Unavailable { .. })),
        "a cancelled run approval fails closed with a typed refusal: {error:?}"
    );

    // The guard's armed drop found no claim: the exclusive slot freed at
    // once, so a second run reaches its approval instead of failing busy.
    assert!(fx.inbox.take().is_none(), "no run claim raced through");
    let second_who = caller("focus", &["run"], Some(turn()));
    let second_workspace = fx.temp.path().to_path_buf();
    let services = Arc::clone(&fx.services);
    let second = spawn(async move {
        services
            .run(&second_who, run_request(&["echo"], second_workspace))
            .await
    });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    let error = second.join().await.unwrap_err();
    assert!(
        matches!(error, ServiceError::Denied(DenyReason::OutOfScope { .. })),
        "the declined fresh approval is a typed refusal: {error:?}"
    );
}

/// A dropped ask reopens only after its withdrawal reaches the durable
/// owner: the next ask is never answered from a stale slot.
#[tokio::test]
async fn a_dropped_ask_reopens_after_its_withdrawal_settles() {
    let fx = fixture(Duration::from_secs(30));
    let ask = |services: Arc<SessionServices>| async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    };
    let first = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    drop(first);
    let claim = loop {
        if let Some(claim) = fx.inbox.take() {
            break claim;
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(
        claim.cause,
        ResolutionCause::Withdrawn,
        "the dropped ask must withdraw as its first resolution: {claim:?}"
    );
    assert!(
        fx.inbox.take().is_none(),
        "the dropped ask must withdraw exactly once"
    );
    // The actor's durable settlement is what frees the slot for reuse.
    fx.inbox.mark_settled(claim.request.id);
    let second = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        second
            .join()
            .await
            .expect("the reopened ask resolves")
            .is_none(),
        "the slot must reopen after the settle"
    );
}

/// Settlement marks of unrelated turn cancellations; more than twice the
/// size of the retired settled-set cap, so any eviction policy keyed on
/// the set's size fires inside one churn.
const SETTLEMENT_CHURN: usize = 8192;

/// Drains `count` real turn-cancel claims through the inbox the way the
/// actor does: every drained claim is marked settled although no services
/// slot waits on it, so the marks are orphans.
fn churn_cancellations(fx: &Fixture, count: usize) {
    let turn = TurnId::new(NonZeroU64::new(2).unwrap());
    for _ in 0..count {
        let (request, _waiter) = fx.broker.open(
            dal_core::Owner::Core,
            Question::Text {
                prompt: "churn?".into(),
                placeholder: None,
            },
            turn,
            tokio::time::Instant::now() + Duration::from_secs(60),
        );
        assert!(fx.broker.report_opened(request.id).is_none());
    }
    fx.inbox.cancel_turn(turn);
    let mut drained = 0;
    while let Some(claim) = fx.inbox.take() {
        assert_eq!(claim.cause, ResolutionCause::Cancelled, "{claim:?}");
        fx.inbox.mark_settled(claim.request.id);
        drained += 1;
    }
    assert_eq!(drained, count, "every cancel claim reached the drain");
}

/// Takes the next claim the inbox queued, yielding until a dropped guard
/// has run its withdrawal.
async fn next_claim(fx: &Fixture) -> crate::broker::Resolved {
    loop {
        if let Some(claim) = fx.inbox.take() {
            return claim;
        }
        tokio::task::yield_now().await;
    }
}

/// A real withdrawal receipt survives unrelated cancellation churn: the
/// slot stays occupied until the actor's durable settlement, and the
/// capacity reopens after it however many orphan marks arrive in between.
/// A forgotten receipt would strand the slot and shrink the battery for
/// good.
#[tokio::test]
async fn a_pending_ask_withdrawal_survives_cancellation_settlement_churn() {
    use super::MAX_OPEN_ASKS;

    let fx = fixture(Duration::from_secs(60));
    let ask = |services: Arc<SessionServices>| async move {
        let who = caller("focus", &["ask"], Some(turn()));
        services
            .ask(
                &who,
                Question::Text {
                    prompt: "proceed?".into(),
                    placeholder: None,
                },
            )
            .await
    };
    let withdrawn = spawn(ask(Arc::clone(&fx.services)));
    await_open(&fx.broker).await;
    let mut live = Vec::new();
    for _ in 1..MAX_OPEN_ASKS {
        live.push(spawn(ask(Arc::clone(&fx.services))));
    }
    for _ in 0..100 {
        if open_count(&fx.broker) >= MAX_OPEN_ASKS {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        open_count(&fx.broker),
        MAX_OPEN_ASKS,
        "the ask battery fills to its cap"
    );

    // The aborted task's armed guard claims a real withdrawal; the claim is
    // unsettled, so its slot stays occupied while unrelated cancellations
    // settle around it.
    drop(withdrawn);
    let claim = next_claim(&fx).await;
    assert_eq!(claim.cause, ResolutionCause::Withdrawn, "{claim:?}");
    churn_cancellations(&fx, 64);
    let busy = fx
        .services
        .ask(
            &caller("focus", &["ask"], Some(turn())),
            Question::Text {
                prompt: "one more?".into(),
                placeholder: None,
            },
        )
        .await;
    assert!(
        matches!(busy, Err(ServiceError::Failed { .. })),
        "unrelated settlements never free an unsettled withdrawal: {busy:?}"
    );

    // Durable settlement, then enough orphan marks to evict any receipt a
    // size-capped set would hold: the capacity must still reopen.
    fx.inbox.mark_settled(claim.request.id);
    churn_cancellations(&fx, SETTLEMENT_CHURN);
    let reopened = spawn(ask(Arc::clone(&fx.services)));
    for _ in 0..100 {
        if open_count(&fx.broker) >= MAX_OPEN_ASKS {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        open_count(&fx.broker),
        MAX_OPEN_ASKS,
        "the settled withdrawal's capacity reopens; a forgotten receipt strands it"
    );
    drop(reopened);
    drop(live);
}

/// The run slot is exclusive, so a forgotten receipt would wedge `run`
/// approvals for the rest of the session: the same churn must leave the
/// settled withdrawal's slot reusable exactly once the actor settles it.
#[tokio::test]
async fn a_pending_run_withdrawal_survives_cancellation_settlement_churn() {
    let fx = fixture(Duration::from_secs(30));
    let workspace = fx.temp.path().to_path_buf();
    {
        // Three runs take the broker approval path: the aborted one, the
        // refused one, and the one admitted after settlement.
        fx.rt.decisions.lock().unwrap().extend([
            dal_core::Decision::Ask { grant: None },
            dal_core::Decision::Ask { grant: None },
            dal_core::Decision::Ask { grant: None },
        ]);
    }
    let who = caller("focus", &["run"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let first_workspace = workspace.clone();
    let first = spawn(async move {
        services
            .run(&who, run_request(&["git", "status"], first_workspace))
            .await
    });
    // The capability grant asks first and persists: approve it so the run
    // reaches its own approval, the slot under test.
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Approve);
    await_open(&fx.broker).await;

    drop(first);
    let claim = next_claim(&fx).await;
    assert_eq!(claim.cause, ResolutionCause::Withdrawn, "{claim:?}");
    churn_cancellations(&fx, 64);
    let busy = fx
        .services
        .run(
            &caller("focus", &["run"], Some(turn())),
            run_request(&["git", "diff"], workspace.clone()),
        )
        .await;
    assert!(
        matches!(busy, Err(ServiceError::Failed { .. })),
        "unrelated settlements never free an unsettled run withdrawal: {busy:?}"
    );

    fx.inbox.mark_settled(claim.request.id);
    churn_cancellations(&fx, SETTLEMENT_CHURN);
    let second_who = caller("focus", &["run"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let second = spawn(async move {
        services
            .run(&second_who, run_request(&["git", "diff"], workspace))
            .await
    });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    let error = second.join().await.unwrap_err();
    assert!(
        matches!(error, ServiceError::Denied(DenyReason::OutOfScope { .. })),
        "the settled withdrawal's run slot reopens to a fresh approval: {error:?}"
    );
}

/// Bounds a call that must be refused before any await. A gate regression
/// falls through to a grant question nobody answers; this turns that into a
/// failure instead of a hung test.
async fn refused_at_once<T>(
    call: impl Future<Output = Result<T, ServiceError>>,
) -> Result<T, ServiceError> {
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the call is refused before any grant question opens")
}

/// Calls every method gated by the `missing` injection, as a caller that lacks
/// only that one, and returns each verdict. `state` rides the sidecar slot, so
/// the sidecar case covers it too.
async fn uninjected_calls(
    fx: &Fixture,
    who: &Caller,
    missing: Service,
) -> Vec<Result<(), ServiceError>> {
    use dal_core::{StateKey, StateNs};
    let services = &fx.services;
    match missing {
        Service::FsRead => vec![
            refused_at_once(services.fs_read(who, "a.txt"))
                .await
                .map(|_| ()),
        ],
        Service::FsWrite => {
            vec![refused_at_once(services.fs_write(who, "a.txt", b"x".to_vec())).await]
        }
        Service::Net => {
            let request = dal_core::FetchRequest {
                method: dal_core::FetchMethod::Get,
                url: "https://example.invalid/".into(),
                headers: Vec::new(),
                body: Vec::new(),
                redirects: dal_core::RedirectPolicy::Follow,
            };
            vec![
                refused_at_once(services.net(who, request))
                    .await
                    .map(|_| ()),
            ]
        }
        Service::Run => {
            let request = run_request(&["git", "status"], fx.temp.path().to_path_buf());
            vec![
                refused_at_once(services.run(who, request))
                    .await
                    .map(|_| ()),
            ]
        }
        Service::Env => vec![refused_at_once(services.env(who, "HOME")).await.map(|_| ())],
        Service::Ask => {
            let question = Question::Text {
                prompt: "proceed?".into(),
                placeholder: None,
            };
            vec![
                refused_at_once(services.ask(who, question))
                    .await
                    .map(|_| ()),
                refused_at_once(services.open_asks(who)).await.map(|_| ()),
            ]
        }
        Service::Mcp => vec![
            refused_at_once(services.mcp(who, mcp_request()))
                .await
                .map(|_| ()),
        ],
        Service::Agents => {
            vec![
                refused_at_once(services.agents(who, AgentsOp::List))
                    .await
                    .map(|_| ()),
            ]
        }
        Service::Jobs => vec![
            refused_at_once(services.jobs(who, JobsOp::List))
                .await
                .map(|_| ()),
        ],
        Service::Turn => {
            vec![
                refused_at_once(services.turn(who, TurnOp::IsIdle))
                    .await
                    .map(|_| ()),
            ]
        }
        Service::Sidecar => {
            let read = StateOp::Read {
                ns: StateNs::Eval,
                key: StateKey::parse("counter").unwrap(),
            };
            vec![
                refused_at_once(services.sidecar(who, sidecar_read()))
                    .await
                    .map(|_| ()),
                refused_at_once(services.state(who, read)).await.map(|_| ()),
            ]
        }
        Service::Infer => {
            vec![refused_at_once(services.authorize_script_infer(who, fx.cancel.clone())).await]
        }
        other => panic!("service {other} has no injection gate under test"),
    }
}

/// Every gated service checks its own injection slot before anything else: a
/// caller that injects every other service is refused with `NotInjected`
/// before a grant question opens or any backend work starts, so no method can
/// borrow a sibling's slot. The loop walks the whole vocabulary, so a new
/// service fails here until its gate is covered.
#[tokio::test]
async fn each_gated_service_refuses_a_caller_that_lacks_its_own_injection() {
    for missing in Service::ALL {
        let fx = fixture(Duration::from_secs(30));
        let injected: Vec<&str> = Service::ALL
            .into_iter()
            .filter(|service| *service != missing)
            .map(Service::as_str)
            .collect();
        let who = caller("focus", &injected, Some(turn()));
        let verdicts = uninjected_calls(&fx, &who, missing).await;
        assert!(!verdicts.is_empty(), "{missing}: no method was exercised");
        for verdict in verdicts {
            assert!(
                matches!(verdict, Err(ServiceError::Denied(DenyReason::NotInjected))),
                "{missing}: an uninjected call is denied for its own slot, got {verdict:?}"
            );
        }
        assert_eq!(
            open_count(&fx.broker),
            0,
            "{missing}: no grant question may open"
        );
        assert!(fx.backend.seen_reads.lock().unwrap().is_empty());
        assert!(fx.backend.seen_env.lock().unwrap().is_empty());
        assert!(fx.backend.state_ops.lock().unwrap().is_empty());
        assert!(fx.backend.updates.lock().unwrap().is_empty());
    }
}

/// `state` rides the sidecar slot and grant: without the slot nothing opens
/// and nothing reaches the backend, and a declined grant reaches no state
/// either.
#[tokio::test]
async fn state_is_gated_by_the_sidecar_slot_and_grant() {
    use dal_core::{StateKey, StateNs};
    let fx = fixture(Duration::from_secs(30));
    let read = || StateOp::Read {
        ns: StateNs::Eval,
        key: StateKey::parse("counter").unwrap(),
    };

    let missing = caller("focus", &["ask"], Some(turn()));
    assert!(matches!(
        refused_at_once(fx.services.state(&missing, read())).await,
        Err(ServiceError::Denied(DenyReason::NotInjected))
    ));
    assert_eq!(open_count(&fx.broker), 0, "no grant question may open");

    let who = caller("focus", &["sidecar"], Some(turn()));
    let services = Arc::clone(&fx.services);
    let declining = who.clone();
    let declined = spawn(async move { services.state(&declining, read()).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Decline);
    assert!(matches!(declined.join().await, Err(ServiceError::Declined)));
    assert!(
        fx.backend.state_ops.lock().unwrap().is_empty(),
        "a refused grant reaches no state"
    );

    let services = Arc::clone(&fx.services);
    let approving = who.clone();
    let approved = spawn(async move { services.state(&approving, read()).await });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::ApproveForSession);
    assert_eq!(
        approved.join().await,
        Ok(Err(StateError::Unavailable)),
        "the unscripted backend answers its own typed failure"
    );
    assert_eq!(
        fx.backend.state_ops.lock().unwrap().len(),
        1,
        "only the approved call reached the backend"
    );
}

/// `state` never trusts the namespace on the operation: the service derives
/// it from the host-minted caller, so a plugin handler cannot reach another
/// plugin's keys or the eval namespace, an eval cell reaches only the eval
/// namespace, and a stale revision comes back as the operation's own typed
/// failure rather than a service error. Builtin handlers and cells skip the
/// capability store, so no grant question is involved.
#[tokio::test]
async fn state_derives_its_namespace_from_the_caller() {
    use dal_core::{Revision, StateKey, StateNs};
    let fx = fixture(Duration::from_secs(30));
    let key = StateKey::parse("counter").unwrap();
    let revision = Revision::new(NonZeroU64::new(3).unwrap());
    let stored = StateRecord {
        present: true,
        value: Some(RawJson::parse("1").unwrap()),
        revision,
    };
    *fx.backend.state_reply.lock().unwrap() = Some(Ok(stored.clone()));
    let version = std::num::NonZeroU32::new(7).unwrap();
    let minted = |origin: Origin, kind: CallerKind| {
        Caller::new(
            "focus".parse::<Name>().unwrap(),
            origin,
            ServiceSet::from_names(["sidecar"]).unwrap(),
            version,
            kind,
            Some(turn()),
        )
        .with_test_digest()
    };
    let handler = minted(Origin::Builtin, CallerKind::Handler);
    let cell = minted(Origin::User, CallerKind::Cell { approved: true });
    // A namespace no caller may reach by naming it.
    let foreign = StateNs::Plugin {
        origin: Origin::Bundled,
        plugin: "other".parse::<Name>().unwrap(),
        version: std::num::NonZeroU32::new(99).unwrap(),
    };
    let own = StateNs::Plugin {
        origin: Origin::Builtin,
        plugin: "focus".parse::<Name>().unwrap(),
        version,
    };
    let eval = StateNs::Eval;
    let written = RawJson::parse("2").unwrap();
    let ops_in = |ns: &StateNs| {
        [
            StateOp::Read {
                ns: ns.clone(),
                key: key.clone(),
            },
            StateOp::Write {
                ns: ns.clone(),
                key: key.clone(),
                value: written.clone(),
                expected: revision,
            },
            StateOp::Delete {
                ns: ns.clone(),
                key: key.clone(),
                expected: revision,
            },
        ]
    };

    for (who, derived) in [(&handler, &own), (&cell, &eval)] {
        fx.backend.state_ops.lock().unwrap().clear();
        for op in ops_in(&foreign) {
            assert_eq!(fx.services.state(who, op).await, Ok(Ok(stored.clone())));
        }
        assert_eq!(
            *fx.backend.state_ops.lock().unwrap(),
            ops_in(derived).to_vec(),
            "the caller never names its namespace"
        );
    }
    assert_eq!(open_count(&fx.broker), 0, "no grant question was involved");

    *fx.backend.state_reply.lock().unwrap() = Some(Err(StateError::Conflict));
    let stale = StateOp::Delete {
        ns: foreign,
        key,
        expected: revision,
    };
    assert_eq!(
        fx.services.state(&handler, stale).await,
        Ok(Err(StateError::Conflict)),
        "a stale revision is the operation's typed failure, not a service error"
    );
}

/// A run the exec ladder refuses outright, or that needs an approval nobody
/// can raise, is a typed denial and never spawns or opens a question: the
/// ladder's own reason passes through unchanged, while "no front end" and "no
/// turn to ask under" take the shared headless denial that front ends parse
/// back.
#[tokio::test]
async fn a_run_nobody_can_approve_is_denied_without_spawning() {
    let fx = fixture(Duration::from_secs(30));
    let workspace = fx.temp.path().to_path_buf();
    // Cells skip the capability store, so only the ladder decides here.
    let cell = |at: Option<TurnId>| {
        Caller::new(
            "focus".parse::<Name>().unwrap(),
            Origin::User,
            ServiceSet::from_names(["run"]).unwrap(),
            std::num::NonZeroU32::MIN,
            CallerKind::Cell { approved: true },
            at,
        )
        .with_test_digest()
    };
    let headless = |error: ServiceError| match error {
        ServiceError::Denied(DenyReason::OutOfScope { what }) => {
            dal_core::parse_headless_denial(&what).map(|(tool, rung)| (tool.to_owned(), rung))
        }
        _ => None,
    };
    fx.rt.decisions.lock().unwrap().extend([
        dal_core::Decision::Deny {
            reason: DenyReason::NoFrontEnd,
        },
        dal_core::Decision::Deny {
            reason: DenyReason::out_of_scope("the ladder refused"),
        },
        dal_core::Decision::Ask { grant: None },
    ]);

    let error = fx
        .services
        .run(
            &cell(Some(turn())),
            run_request(&["git"], workspace.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        headless(error),
        Some(("run".to_owned(), dal_core::Rung::All)),
        "a ladder with no front end takes the shared headless denial"
    );

    let error = fx
        .services
        .run(
            &cell(Some(turn())),
            run_request(&["git"], workspace.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ServiceError::Denied(DenyReason::out_of_scope("the ladder refused")),
        "any other ladder reason passes through unchanged"
    );

    let error = fx
        .services
        .run(&cell(None), run_request(&["git"], workspace))
        .await
        .unwrap_err();
    assert_eq!(
        headless(error),
        Some(("run".to_owned(), dal_core::Rung::All)),
        "an approval with no turn to raise it under is headless"
    );
    assert_eq!(open_count(&fx.broker), 0, "no question opened for any run");
    assert!(fx.rt.previews.lock().unwrap().is_empty());
    assert!(fx.rt.spawns.lock().unwrap().is_empty(), "nothing spawned");
}

/// One released ask settlement maps onto exactly one typed outcome: a value
/// passes through, a Confirm's approve and decline become booleans, a
/// dismissal or a deadline default is "no answer", an approval verdict on any
/// other question fails closed to "no answer", and only a cancelled request is
/// an error.
#[test]
fn ask_settlements_map_onto_typed_outcomes() {
    let settle = |resolution: Resolution, answer: Answer, confirm: bool| {
        SessionServices::settle_ask_answer(
            crate::broker::Settled {
                answer,
                by: ClientId::new("test-front-end"),
                resolution,
                cause: None,
            },
            confirm,
        )
    };
    let json = |text: &str| Answer::Value(RawJson::parse(text).unwrap());

    for confirm in [false, true] {
        assert_eq!(
            settle(Resolution::Answered, json("\"pick\""), confirm),
            Ok(Some(json("\"pick\""))),
            "a value answer passes through"
        );
        assert_eq!(
            settle(Resolution::Answered, Answer::Cancel, confirm),
            Ok(None),
            "a dismissal is no answer"
        );
        assert_eq!(
            settle(Resolution::Unavailable, Answer::Cancel, confirm),
            Ok(None),
            "a deadline default is no answer"
        );
        assert_eq!(
            settle(Resolution::Cancelled, Answer::Cancel, confirm),
            Err(ServiceError::Cancelled),
            "a cancelled request is the one error"
        );
    }
    assert_eq!(
        settle(Resolution::Answered, Answer::Approve, true),
        Ok(Some(json("true"))),
        "a Confirm approval is true"
    );
    assert_eq!(
        settle(Resolution::Answered, Answer::Decline, true),
        Ok(Some(json("false"))),
        "a Confirm decline is false"
    );
    assert_eq!(
        settle(Resolution::Answered, Answer::ApproveForSession, true),
        Ok(None),
        "a session approval is not a Confirm answer"
    );
    for verdict in [Answer::Approve, Answer::ApproveForSession, Answer::Decline] {
        assert_eq!(
            settle(Resolution::Answered, verdict, false),
            Ok(None),
            "an approval verdict on a non-Confirm question fails closed"
        );
    }
}

/// Owner equality is the whole private-outcome authority, so no two live
/// origins may render to the same owner text, and none may take the shared
/// fallback spelling.
#[test]
fn every_live_origin_renders_to_its_own_owner_text() {
    let texts: std::collections::HashSet<&str> = [Origin::Builtin, Origin::Bundled, Origin::User]
        .into_iter()
        .map(super::origin_text_shared)
        .collect();
    assert_eq!(texts.len(), 3, "origins never alias one owner: {texts:?}");
    assert!(
        !texts.contains("unknown"),
        "a live origin never takes the fallback spelling: {texts:?}"
    );
}

/// Mints a host-shaped caller for private-outcome reads: the authority under
/// test is the (name, origin) pair, never the inject set.
fn private_caller(name: &str, origin: Origin) -> Caller {
    Caller::new(
        name.parse::<Name>().unwrap(),
        origin,
        ServiceSet::from_names(["ask"]).unwrap(),
        std::num::NonZeroU32::MIN,
        CallerKind::Handler,
        Some(turn()),
    )
    .with_test_digest()
}

fn private_shared() -> Shared {
    Shared::new(
        false,
        Gen::new(std::num::NonZeroU64::MIN),
        ThinkingLevel::Medium,
        ApprovalMode::Ask,
        Mode::Normal,
    )
}

/// Reads exactly the way [`crate::session::backend::Backend::private_outcomes`]
/// does — owner derived from the real caller exactly as the backend does,
/// predicate inside the shared lock and mapping outside — but over the
/// test's own real `Shared`, since a full backend needs host state no unit
/// test can mint.
fn read_private(shared: &Shared, caller: &Caller) -> Vec<PrivateEffect> {
    let owner = private_owner_of(caller);
    shared
        .private_terminal_select(|terminal| terminal.who == owner)
        .iter()
        .map(|terminal| PrivateEffect::from(terminal.as_ref()))
        .collect()
}

/// One settled loop journal: birth, one start, one end.
fn private_settled_loop(
    owner: Owner,
    round: u32,
    name: &str,
) -> (PrivateLoopId, PrivateEffectId, Vec<Record>) {
    let at = jiff::Timestamp::UNIX_EPOCH;
    let loop_id = PrivateLoopId::new_v7();
    let token = PrivateEffectId::new_v7();
    let records = vec![
        Record::PrivateLoop {
            at,
            r#loop: loop_id,
            who: owner.clone(),
            turn: Some(turn()),
        },
        Record::PrivateStart {
            at,
            r#loop: loop_id,
            token,
            who: owner,
            turn: Some(turn()),
            round,
            call: CallId::new("wire-call-1"),
            name: name.into(),
            args: RawJson::parse(r#"{"n":1}"#).unwrap(),
        },
        Record::PrivateEnd {
            at,
            r#loop: loop_id,
            token,
            outcome: PrivateOutcome::Settled {
                is_error: false,
                parts: vec![JournalPart::Text { text: "ok".into() }],
            },
        },
    ];
    (loop_id, token, records)
}

#[test]
fn private_outcomes_return_only_the_calling_owner_terminals() {
    let user_caller = private_caller("focus", Origin::User);
    let bundled_caller = private_caller("focus", Origin::Bundled);
    let stranger_caller = private_caller("stranger", Origin::User);
    let user_owner = private_owner_of(&user_caller);
    let bundled_owner = private_owner_of(&bundled_caller);
    let (user_loop, user_token, mut records) = private_settled_loop(user_owner, 2, "tool-a");
    let (bundled_loop, bundled_token, mut bundled_records) =
        private_settled_loop(bundled_owner, 0, "tool-b");
    records.append(&mut bundled_records);
    // A hand-journaled core-owned row is fail-closed: no extension reads it.
    let core_loop = PrivateLoopId::new_v7();
    let core_token = PrivateEffectId::new_v7();
    let at = jiff::Timestamp::UNIX_EPOCH;
    records.extend([
        Record::PrivateLoop {
            at,
            r#loop: core_loop,
            who: Owner::Core,
            turn: None,
        },
        Record::PrivateStart {
            at,
            r#loop: core_loop,
            token: core_token,
            who: Owner::Core,
            turn: None,
            round: 0,
            call: CallId::new("core-call"),
            name: "core-tool".into(),
            args: RawJson::parse("null").unwrap(),
        },
        Record::PrivateEnd {
            at,
            r#loop: core_loop,
            token: core_token,
            outcome: PrivateOutcome::Indeterminate,
        },
    ]);
    let (fold, _) = Session::replay(records, at).expect("private records replay");
    let shared = private_shared();
    shared.sync_ext(&fold);

    // The index holds every terminal; each owner reads only its own.
    assert_eq!(
        shared.private_terminal_select(|_| true).len(),
        3,
        "both owner terminals plus the core row publish"
    );
    let user_rows = read_private(&shared, &user_caller);
    assert_eq!(user_rows.len(), 1, "user/focus reads only its terminal");
    let row = &user_rows[0];
    assert_eq!(row.r#loop, user_loop);
    assert_eq!(row.token, user_token);
    assert_eq!(row.turn, Some(turn()));
    assert_eq!(row.round, 2);
    assert_eq!(&*row.name, "tool-a");
    assert!(
        matches!(
            &row.outcome,
            PrivateOutcome::Settled { is_error: false, parts }
            if parts == &vec![JournalPart::Text { text: "ok".into() }]
        ),
        "settled payload survives the shared Arc: {:?}",
        row.outcome
    );
    let bundled_rows = read_private(&shared, &bundled_caller);
    assert_eq!(
        bundled_rows.len(),
        1,
        "same name under bundled origin reads only its own"
    );
    assert_eq!(bundled_rows[0].r#loop, bundled_loop);
    assert_eq!(bundled_rows[0].token, bundled_token);
    assert!(
        read_private(&shared, &stranger_caller).is_empty(),
        "a third owner reads nothing"
    );
}

#[test]
fn private_end_only_advance_publishes_without_a_leaf_move() {
    let caller = private_caller("focus", Origin::User);
    let owner = private_owner_of(&caller);
    let at = jiff::Timestamp::UNIX_EPOCH;
    let (_, _, mut records) = private_settled_loop(owner.clone(), 0, "tool-a");
    let (fold_a, _) = Session::replay(records.clone(), at).expect("first replay");
    let shared = private_shared();
    shared.sync_ext(&fold_a);
    assert_eq!(read_private(&shared, &caller).len(), 1);

    let (_, _, mut second) = private_settled_loop(owner, 1, "tool-b");
    records.append(&mut second);
    let (fold_b, _) = Session::replay(records, at).expect("second replay");
    // The staleness-trap condition: settling a private row moves neither the
    // leaf nor the ext length, so a publication placed after the sync_ext
    // memo early-return would stay stuck at one row.
    assert_eq!(fold_b.leaf_entry(), fold_a.leaf_entry());
    assert_eq!(fold_b.ext_len(), fold_a.ext_len());
    shared.sync_ext(&fold_b);
    let rows = read_private(&shared, &caller);
    assert_eq!(rows.len(), 2, "the End-only advance still publishes");
    assert_eq!(&*rows[0].name, "tool-a");
    assert_eq!(
        &*rows[1].name, "tool-b",
        "terminal order follows End journal order"
    );
}

#[test]
fn private_unfinished_effect_never_publishes_a_settled_row() {
    let caller = private_caller("focus", Origin::User);
    let owner = private_owner_of(&caller);
    let at = jiff::Timestamp::UNIX_EPOCH;
    let loop_id = PrivateLoopId::new_v7();
    let token = PrivateEffectId::new_v7();
    // A Start with no End: the effect never settled (failed append, crash
    // before the End write). Replay honestly repairs it to Indeterminate —
    // but no Settled row may ever appear for it.
    let (fold, _) = Session::replay(
        [
            Record::PrivateLoop {
                at,
                r#loop: loop_id,
                who: owner.clone(),
                turn: Some(turn()),
            },
            Record::PrivateStart {
                at,
                r#loop: loop_id,
                token,
                who: owner,
                turn: Some(turn()),
                round: 3,
                call: CallId::new("wire-call-9"),
                name: "tool-a".into(),
                args: RawJson::parse(r#"{"n":9}"#).unwrap(),
            },
        ],
        at,
    )
    .expect("unfinished private loop replays");
    assert_eq!(
        fold.private_terminal_len(),
        1,
        "the Start itself publishes nothing; only the repair terminal exists"
    );
    let shared = private_shared();
    shared.sync_ext(&fold);
    let rows = read_private(&shared, &caller);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].r#loop, loop_id);
    assert_eq!(rows[0].token, token);
    assert_eq!(rows[0].round, 3);
    assert!(
        matches!(rows[0].outcome, PrivateOutcome::Indeterminate),
        "an unfinished effect reads Indeterminate, never Settled: {:?}",
        rows[0].outcome
    );
}

#[tokio::test]
async fn private_outcomes_forward_needs_no_inject_or_grant() {
    let fx = fixture(Duration::from_secs(30));
    let loop_id = PrivateLoopId::new_v7();
    fx.backend
        .private_outcomes_reply
        .lock()
        .unwrap()
        .push(PrivateEffect {
            r#loop: loop_id,
            token: PrivateEffectId::new_v7(),
            turn: Some(turn()),
            round: 0,
            name: "tool-a".into(),
            outcome: PrivateOutcome::Indeterminate,
        });
    // An empty inject set: an own-data read must not consult grants.
    let who = caller("focus", &[], Some(turn()));
    let rows = fx
        .services
        .private_outcomes(&who)
        .await
        .expect("private outcomes forward");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].r#loop, loop_id);
}

#[tokio::test]
async fn infer_forwards_caller_continuation() {
    let fx = fixture(Duration::from_secs(30));
    *fx.backend.infer_reply.lock().unwrap() = Some(Inference { events: Vec::new() });

    let owner = crate::session::continuation::ContinuationOwner::new(
        crate::session::tasks::SessionTasks::new(),
    );
    let weak = owner.downgrade();
    let expected_slot = weak.upgrade().expect("owner is alive");

    let who_with_slot = caller("focus", &[], Some(turn())).with_continuation(weak.clone());
    fx.services
        .infer(&who_with_slot, model_request())
        .await
        .expect("infer succeeds");
    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed infer caller");
    let recorded_weak = recorded
        .continuation()
        .expect("recorded caller has continuation");
    assert!(std::sync::Weak::ptr_eq(&recorded_weak, &weak));
    let upgraded_slot = recorded_weak
        .upgrade()
        .expect("slot lives while owner lives");
    assert!(Arc::ptr_eq(&upgraded_slot, &expected_slot));

    let who_without_slot = caller("focus", &[], Some(turn()));
    fx.services
        .infer(&who_without_slot, model_request())
        .await
        .expect("infer succeeds");
    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed infer caller");
    assert!(
        recorded.continuation().is_none(),
        "services must not invent a continuation when caller has none"
    );

    let stream_who_with_slot = caller("focus", &[], Some(turn())).with_continuation(weak.clone());
    let _stream = fx
        .services
        .infer_stream(&stream_who_with_slot, model_request())
        .await
        .expect("infer_stream succeeds");
    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed infer_stream caller");
    let recorded_weak = recorded
        .continuation()
        .expect("recorded caller has continuation");
    assert!(std::sync::Weak::ptr_eq(&recorded_weak, &weak));
    let upgraded_slot = recorded_weak
        .upgrade()
        .expect("slot lives while owner lives");
    assert!(Arc::ptr_eq(&upgraded_slot, &expected_slot));

    let stream_who_without_slot = caller("focus", &[], Some(turn()));
    let _stream = fx
        .services
        .infer_stream(&stream_who_without_slot, model_request())
        .await
        .expect("infer_stream succeeds");
    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed infer_stream caller");
    assert!(
        recorded.continuation().is_none(),
        "services must not invent a continuation when caller has none"
    );
}

#[tokio::test]
async fn script_infer_forwards_caller_continuation() {
    let fx = fixture(Duration::from_secs(30));
    *fx.backend.infer_reply.lock().unwrap() = Some(Inference { events: Vec::new() });

    let owner = crate::session::continuation::ContinuationOwner::new(
        crate::session::tasks::SessionTasks::new(),
    );
    let weak = owner.downgrade();
    let expected_slot = weak.upgrade().expect("owner is alive");

    let gated_with_slot = caller("focus", &["infer"], Some(turn())).with_continuation(weak.clone());
    let services = Arc::clone(&fx.services);
    let cancel = fx.cancel.clone();
    let who = gated_with_slot.clone();
    let approved = spawn(async move {
        services
            .script_infer_with(&who, model_request(), None, cancel)
            .await
    });
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::ApproveForSession);
    approved.join().await.expect("approved script infer runs");

    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed script_infer_with caller");
    let recorded_weak = recorded
        .continuation()
        .expect("recorded caller has continuation");
    assert!(std::sync::Weak::ptr_eq(&recorded_weak, &weak));
    let upgraded_slot = recorded_weak
        .upgrade()
        .expect("slot lives while owner lives");
    assert!(Arc::ptr_eq(&upgraded_slot, &expected_slot));

    let gated_without_slot = caller("focus", &["infer"], Some(turn()));
    fx.services
        .script_infer_with(
            &gated_without_slot,
            model_request(),
            None,
            fx.cancel.clone(),
        )
        .await
        .expect("reused grant allows script_infer_with without asking");
    let recorded = fx
        .backend
        .seen_infer_callers
        .lock()
        .unwrap()
        .pop()
        .expect("backend observed script_infer_with caller");
    assert!(
        recorded.continuation().is_none(),
        "services must not invent a continuation when caller has none"
    );
}
