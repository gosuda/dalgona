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
use dal_core::ext::{McpBlock, McpDeclaration, McpServerDecl};
use dal_core::{
    AgentsOp, AgentsReply, Answer, Budget, CallId, ClientId, DenyReason, EntryId, Inference, JobId,
    JobsOp, JobsReply, ModelRequest, ModelRoute, Name, OnError, Origin, Purpose, Question, RawJson,
    RequestParams, ScopeSpec, Service, ServiceSet, SessionId, SidecarName, SidecarOp, Site,
    StateError, StateOp, StateRecord, TurnId, TurnOp, TurnOpReply, Workspace,
};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::SessionServicesDeps;
use super::{Caller, CallerKind, ServiceFuture, Services, SessionBackend, SessionServices};
use crate::Broker;
use crate::error::ServiceError;
use crate::ext::Doc;
use crate::ext::generation::{Generation, ValidatedExtensions};
use crate::ext::grants::GrantStore;
use crate::ext::tool::{Approved, ToolCxRuntime};
use crate::ext::{Extension, ExtensionBuilder};
use crate::ext::{Scope, ScopeError};
use crate::proc::Proc;

/// One canned reply per backend operation, in call order.
#[derive(Default)]
struct FakeBackend {
    agents_reply: Mutex<Option<AgentsReply>>,
    jobs_reply: Mutex<Option<JobsReply>>,
    turn_reply: Mutex<Option<TurnOpReply>>,
    sidecar_value: Mutex<Option<Vec<u8>>>,
    infer_reply: Mutex<Option<Inference>>,
    seen_env: Mutex<Vec<String>>,
    env_value: Mutex<Option<String>>,
    rows: Mutex<Vec<crate::ext::ExtRecord>>,
    blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    updates: Mutex<Vec<dal_core::UpdateKind>>,
    /// Resolutions the ask guard asked the actor to journal.
    resolved: Mutex<Vec<crate::broker::Resolved>>,
    headless: std::sync::atomic::AtomicBool,
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
    fn blob_put(&self, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        let digest = *blake3::hash(&bytes).as_bytes();
        self.blobs.lock().unwrap().entry(digest).or_insert(bytes);
        Box::pin(async move { Ok(digest) })
    }

    fn blob_get(&self, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let bytes = self.blobs.lock().unwrap().get(&digest).cloned();
        Box::pin(async move { Ok(bytes) })
    }

    fn fs_read(&self, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move { unreachable!("this test never reads files") })
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

    fn state(&self, _op: StateOp) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        Box::pin(async move { Ok(Err(StateError::Unavailable)) })
    }

    fn infer(&self, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        let reply = self
            .infer_reply
            .lock()
            .unwrap()
            .clone()
            .expect("infer is scripted");
        Box::pin(async move { Ok(reply) })
    }

    fn infer_stream(&self, _req: ModelRequest) -> ServiceFuture<'_, dal_provider::EventStream> {
        Box::pin(async move { unreachable!("this test never streams") })
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
    fn request_resolved(&self, resolved: crate::broker::Resolved) {
        self.resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(resolved);
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
            spawns: Mutex::new(Vec::new()),
            workspace: Workspace::new(std::env::temp_dir()).expect("test workspace"),
            cancel: CancellationToken::new(),
        }
    }
}

impl ToolCxRuntime for FakeRt {
    fn decide_run(&self) -> dal_core::Decision {
        dal_core::Decision::Allow
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
    backend: Arc<FakeBackend>,
    rt: Arc<FakeRt>,
    temp: tempfile::TempDir,
    cancel: CancellationToken,
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
        GrantStore::with_runtime(
            temp.path().to_path_buf(),
            Duration::from_secs(120),
            Arc::clone(&broker),
        )
        .unwrap(),
    );
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
        cancel: cancel.clone(),
        ask_timeout,
        ephemeral,
        workspace,
    }));
    Fixture {
        services,
        broker,
        backend,
        rt,
        temp,
        cancel,
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

/// Drops the oldest open broker question unsigned, for timeout cases whose
/// waiter is already gone.
fn abandon_next(broker: &Broker) {
    let id = broker
        .state
        .lock()
        .unwrap()
        .open_order
        .pop_front()
        .expect("an open request");
    broker
        .state
        .lock()
        .unwrap()
        .slots
        .remove(&id)
        .expect("its slot");
}

fn open_count(broker: &Broker) -> usize {
    broker.state.lock().unwrap().open_order.len()
}

/// Waits until the services open one broker question.
async fn await_open(broker: &Broker) {
    for _ in 0..100 {
        if open_count(broker) > 0 {
            return;
        }
        sleep(Duration::from_millis(10)).await;
    }
    panic!("no question opened");
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

#[tokio::test]
async fn ask_queue_returns_value_none_and_headless_second_ask_error() {
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

    // A broker cancellation interrupts the open question.
    let third = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    answer_next(&fx.broker, Answer::Cancel);
    assert!(matches!(third.join().await, Err(ServiceError::Cancelled)));

    // With no answer at all the ask timeout resolves to no answer.
    let timed_out = spawn(ask(Arc::clone(&fx.services), turn()));
    assert!(
        timed_out
            .join()
            .await
            .expect("a timeout resolves")
            .is_none()
    );
    // The timed-out question stays open on the broker; drop it unsigned.
    abandon_next(&fx.broker);

    // A second simultaneous ask fails while the first is still open.
    let pending = spawn(ask(Arc::clone(&fx.services), turn()));
    await_open(&fx.broker).await;
    let busy = fx
        .services
        .ask(
            &caller("focus", &["ask"], Some(turn())),
            Question::Text {
                prompt: "second?".into(),
                placeholder: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        busy,
        ServiceError::Failed {
            service: Some(Service::Ask),
            ..
        }
    ));
    assert_eq!(
        busy.to_string(),
        "another question is already open in this front end",
    );
    answer_next(&fx.broker, Answer::Decline);
    assert!(
        pending
            .join()
            .await
            .expect("the first ask resolves")
            .is_none()
    );

    // A turn cancellation interrupts the open question.
    let cancelled_fx = fixture(Duration::from_secs(30));
    let waiting = spawn(ask(Arc::clone(&cancelled_fx.services), turn()));
    await_open(&cancelled_fx.broker).await;
    cancelled_fx.cancel.cancel();
    assert!(matches!(waiting.join().await, Err(ServiceError::Cancelled)));
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
            assert_eq!(&*what, "MCP client");
            assert_eq!(
                super::MCP_UNAVAILABLE_TEXT,
                "the mcp service needs an MCP client; dalgon has none; \
                 dalgona configures one under [mcp]",
            );
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

    let web = block_of(
        "web",
        McpServerDecl::Http {
            url: "https://docs.example/mcp".into(),
        },
    );
    let files = block_of(
        "files",
        McpServerDecl::Stdio {
            command: vec!["npx".into(), "files-server".into()],
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
            name: "member".into(),
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
    let resolved = fx.backend.resolved.lock().unwrap();
    assert!(
        resolved
            .iter()
            .any(|r| r.answer == dal_core::Answer::Cancel),
        "the retired question was routed to the actor as Cancel: {resolved:?}"
    );
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
