#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real process and filesystem boundaries"
)]

//! Full-load session lifecycle: hooks, phases, actor scheduling, and shutdown races.
#[expect(
    dead_code,
    reason = "gate helpers are shared across integration targets"
)]
mod support;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use dal_agent::{
    Agent, Delivery, Env, Host, Product, SessionRef,
    ext::{
        BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
        ModelHandler, ModelRecord, ObserveHook, ScopeValue, StreamWatch, WatchFactory,
    },
};
use dal_core::ext::{BeforeRequest, BeforeTurn, TurnEnd};
use dal_core::{
    AgentReport, AgentStart, Budget, CallId, CancelScope, Caps, ClientId, Command, Config,
    ConfigProduct, ContextItem, Effect, EntryKind, Event, Expect, HookEvent, HookOutcome,
    HookVerdict, InputEvent, InputVerdict, ModelId, ModelRequest, ModelRoute, OnError, PageReq,
    Part, Phase, Product as StoreProduct, RawJson, Record, Reply, Save, ScopeSpec, ServiceSet,
    Session, SessionEnd, SessionId, SessionStart, Settled, Stop, StreamVerdict, ToolCallEvent,
    ToolCallVerdict, ToolResultEvent, TurnId, TurnState, Usage, Workspace,
};
use dal_provider::{EventStream, StopReason, StreamEvent, ToolArgs, ToolCall};
use dal_store::Store;
use futures::{SinkExt, StreamExt, future::join_all, stream};
use proptest::prelude::*;
use shuttle::{future as shuttle_future, sync::Mutex as ShuttleMutex};
use sonic_rs::JsonValueTrait;
use support::TestDir;

use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::Message};

const CHILD_SESSIONS: usize = 501;
const PROCESS_JOBS: usize = 201;
const CANCELLATIONS: usize = 100;
const WEBSOCKET_CLIENTS: usize = 8;
const RESOURCE_LIMIT_BYTES: u64 = 256 * 1024 * 1024;
// ~700 live sessions hold ≈3 handles each (journal, job log, workspace
// dir) plus harness overhead; the end-of-scenario parity check against the
// pre-run count is the actual leak guard — this bounds unbounded growth.
const HANDLE_LIMIT: usize = 4096;
const CANCEL_P99: Duration = Duration::from_millis(250);
const ROOT_MODEL: &str = "gate-stress/children";
const NESTED_MODEL: &str = "gate-stress/nested";
const LEAF_MODEL: &str = "gate-stress/leaf";
const MEMBER_MODEL: &str = "gate-stress/member";
const JOB_MODEL: &str = "gate-stress/job";
const IDLE_MODEL: &str = "gate-stress/idle";
const SHUTTLE_ROOT_MODEL: &str = "gate-stress/shuttle-root";
const SHUTTLE_SLOW_MODEL: &str = "gate-stress/shuttle-slow";

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestError = Box<dyn Error + Send + Sync>;

#[derive(Default)]
struct HookCounts {
    session_starts: AtomicUsize,
    session_ends: AtomicUsize,
    inputs: AtomicUsize,
    before_turns: AtomicUsize,
    before_requests: AtomicUsize,
    tool_calls: AtomicUsize,
    tool_results: AtomicUsize,
    turn_ends: AtomicUsize,
    turn_ends_by_session: Mutex<HashMap<SessionId, usize>>,
    settled: AtomicUsize,
    stream_starts: AtomicUsize,
    stream_deltas: AtomicUsize,
    stream_finishes: AtomicUsize,
    ends_by_session: Mutex<HashMap<SessionId, usize>>,
    children_by_session: Mutex<Vec<SessionId>>,
}

struct CountingHooks(Arc<HookCounts>);

impl ObserveHook<SessionStart> for CountingHooks {
    fn call(&self, _event: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.session_starts.fetch_add(1, Ordering::Relaxed);
        if cx.parent.is_some() {
            locked(&self.0.children_by_session).push(cx.session);
        }
        Box::pin(async { Ok(()) })
    }
}

impl ObserveHook<SessionEnd> for CountingHooks {
    fn call(&self, _event: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.session_ends.fetch_add(1, Ordering::Relaxed);
        *locked(&self.0.ends_by_session)
            .entry(cx.session)
            .or_default() += 1;
        Box::pin(async { Ok(()) })
    }
}

impl Hook<InputEvent, InputVerdict> for CountingHooks {
    fn call(
        &self,
        _event: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        self.0.inputs.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(InputVerdict::Continue) })
    }
}

impl Hook<BeforeTurn, Option<String>> for CountingHooks {
    fn call(
        &self,
        _event: BeforeTurn,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        self.0.before_turns.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(None) })
    }
}

impl Hook<BeforeRequest, Option<dal_core::RequestParams>> for CountingHooks {
    fn call(
        &self,
        _event: BeforeRequest,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<dal_core::RequestParams>, HookError>> {
        self.0.before_requests.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(None) })
    }
}

impl Hook<ToolCallEvent, ToolCallVerdict> for CountingHooks {
    fn call(
        &self,
        _event: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        self.0.tool_calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(ToolCallVerdict::Allow) })
    }
}

impl ObserveHook<ToolResultEvent> for CountingHooks {
    fn call(
        &self,
        _event: ToolResultEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.tool_results.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    }
}

impl ObserveHook<TurnEnd> for CountingHooks {
    fn call(&self, _event: TurnEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.turn_ends.fetch_add(1, Ordering::Relaxed);
        *locked(&self.0.turn_ends_by_session)
            .entry(cx.session)
            .or_default() += 1;
        Box::pin(async { Ok(()) })
    }
}

impl ObserveHook<Settled> for CountingHooks {
    fn call(&self, _event: Settled, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.settled.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    }
}

struct CountingWatchFactory(Arc<HookCounts>);

impl WatchFactory for CountingWatchFactory {
    fn start(&self, _turn: &dal_agent::ext::TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        self.0.stream_starts.fetch_add(1, Ordering::Relaxed);
        Some(Box::new(CountingWatch(Arc::clone(&self.0))))
    }
}

struct CountingWatch(Arc<HookCounts>);

impl StreamWatch for CountingWatch {
    fn feed(&mut self, _channel: dal_core::Channel, _delta: &str) -> StreamVerdict {
        self.0.stream_deltas.fetch_add(1, Ordering::Relaxed);
        StreamVerdict::Continue
    }

    fn finish(&mut self) -> StreamVerdict {
        self.0.stream_finishes.fetch_add(1, Ordering::Relaxed);
        StreamVerdict::Continue
    }
}

struct ChildLoadModel {
    reports: Arc<Mutex<Vec<AgentReport>>>,
}

impl ModelHandler for ChildLoadModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let Ok(scope) = cx.scope(scope_spec(64)) else {
                return Err(ModelError::PrivateRounds);
            };
            let mut nested_request = request.clone();
            nested_request.model = synthetic_route(NESTED_MODEL);
            let Ok(nested) = scope.infer(nested_request) else {
                return Err(ModelError::PrivateRounds);
            };
            for index in 0..CHILD_SESSIONS {
                let child = AgentStart {
                    call: CallId::new(format!("child-call-{index}")),
                    name: format!("member-{index}").into(),
                    prompt: format!("report from member {index}").into(),
                    model: Some(MEMBER_MODEL.into()),
                    role: None,
                    system: None,
                    tools: Some(Box::default()),
                    workspace: None,
                };
                if scope.agent(child).is_err() {
                    return Err(ModelError::PrivateRounds);
                }
            }
            let handles = scope.all().await;
            let mut reports = Vec::with_capacity(CHILD_SESSIONS);
            for handle in handles {
                match handle.result().await {
                    Ok(ScopeValue::Agent(report)) => reports.push(report),
                    // The nested inference sits first in creation order; it
                    // is awaited again below, so it is not a failure.
                    Ok(ScopeValue::Inference(_)) => {}
                    _ => return Err(ModelError::PrivateRounds),
                }
            }
            if reports.len() != CHILD_SESSIONS {
                return Err(ModelError::PrivateRounds);
            }
            if !matches!(nested.result().await, Ok(ScopeValue::Inference(_))) {
                return Err(ModelError::PrivateRounds);
            }
            locked(&self.reports).extend(reports);
            Ok(text_stream("children complete"))
        })
    }
}

struct NestedModel;

impl ModelHandler for NestedModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let Ok(scope) = cx.scope(scope_spec(2)) else {
                return Err(ModelError::PrivateRounds);
            };
            for _ in 0..4 {
                let mut leaf_request = request.clone();
                leaf_request.model = synthetic_route(LEAF_MODEL);
                if scope.infer(leaf_request).is_err() {
                    return Err(ModelError::PrivateRounds);
                }
            }
            for handle in scope.all().await {
                if !matches!(handle.result().await, Ok(ScopeValue::Inference(_))) {
                    return Err(ModelError::PrivateRounds);
                }
            }
            Ok(text_stream("nested complete"))
        })
    }
}

struct MemberModel;

impl ModelHandler for MemberModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move { Ok(text_stream("member complete")) })
    }
}

struct LeafModel {
    runs: Arc<AtomicUsize>,
}

impl ModelHandler for LeafModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.runs.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(text_stream("leaf complete")) })
    }
}

struct IdleModel {
    starts: Arc<AtomicUsize>,
}

impl ModelHandler for IdleModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(EventStream::new(stream::pending(), || {})) })
    }
}

struct JobModel;

impl ModelHandler for JobModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if request.context.iter().any(|item| {
                matches!(item, ContextItem::ToolResult { name, .. } if name.as_ref() == "exec")
            }) {
                return Ok(text_stream("process started"));
            }
            let Some(index) = request
                .context
                .iter()
                .filter_map(user_text)
                .find_map(|text| {
                    text.strip_prefix("process-job-")
                        .and_then(|number| number.parse::<usize>().ok())
                })
            else {
                return Err(ModelError::PrivateRounds);
            };
            let call = format!("process-{index}");
            let args = format!(
                r#"{{"command":"{}","timeout_seconds":600,"foreground_s":300}}"#,
                process_command(index)
            );
            Ok(tool_call_stream(&call, &args))
        })
    }
}

struct ShuttleRootModel {
    mail_results: Arc<Mutex<Vec<(String, dal_core::Receipt)>>>,
    completed_sends: Arc<AtomicUsize>,
}

impl ModelHandler for ShuttleRootModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let Ok(scope) = cx.scope(scope_spec(1)) else {
                return Err(ModelError::PrivateRounds);
            };
            let child = AgentStart {
                call: CallId::new("shuttle-child-call"),
                name: "shuttle-child".into(),
                prompt: "hold a member turn while mail arrives".into(),
                model: Some(SHUTTLE_SLOW_MODEL.into()),
                role: None,
                system: None,
                tools: Some(Box::default()),
                workspace: None,
            };
            let Ok(handle) = scope.agent(child) else {
                return Err(ModelError::PrivateRounds);
            };
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            for index in 0..32 {
                let text = format!("mail-{index}");
                match handle.send(&text, dal_core::MailMode::Aside).await {
                    Ok(receipt) => {
                        locked(&self.mail_results).push((text, receipt));
                        self.completed_sends.fetch_add(1, Ordering::Release);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(_) => return Err(ModelError::PrivateRounds),
                }
            }
            std::future::pending::<()>().await;
            Ok(text_stream("unreachable"))
        })
    }
}

struct ShuttleSlowModel {
    starts: Arc<AtomicUsize>,
}

impl ModelHandler for ShuttleSlowModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.starts.fetch_add(1, Ordering::Release);
        Box::pin(async { Ok(EventStream::new(stream::pending(), || {})) })
    }
}

struct ChildSessionHook(Arc<HookCounts>);

impl ObserveHook<SessionStart> for ChildSessionHook {
    fn call(&self, _event: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        if cx.parent.is_some() {
            locked(&self.0.children_by_session).push(cx.session);
        }
        Box::pin(async { Ok(()) })
    }
}

struct ServerProcess(Option<Child>);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct ProcessJob {
    agent: Agent,
    updates: dal_agent::Subscription,
    turn: TurnId,
    pid_path: PathBuf,
    pid: Option<u32>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn caps(tool_use: bool) -> Caps {
    Caps {
        context_window: Some(8192),
        thinking: Box::default(),
        tool_use,
        image_input: false,
        custom_grammar: false,
    }
}

fn scope_spec(limit: u16) -> ScopeSpec {
    ScopeSpec {
        limit,
        on_error: OnError::Settle,
        budget: Budget::default(),
    }
}

fn synthetic_route(id: &str) -> ModelRoute {
    ModelRoute::Synthetic { id: id.into() }
}

fn text_stream(text: &str) -> EventStream {
    let events = vec![
        Ok(StreamEvent::TextDelta {
            text: text.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ];
    EventStream::new(stream::iter(events), || {})
}

fn tool_call_stream(call: &str, args: &str) -> EventStream {
    let raw = RawJson::parse(args).expect("generated exec arguments are valid JSON");
    let event = ToolCall {
        id: call.to_owned(),
        name: "exec".to_owned(),
        args: ToolArgs::Parsed(raw),
    };
    let events = vec![
        Ok(StreamEvent::TextDelta {
            text: "starting process".to_owned(),
        }),
        Ok(StreamEvent::ToolCallStarted {
            id: call.to_owned(),
            name: "exec".to_owned(),
        }),
        Ok(StreamEvent::ToolArgsDelta {
            id: call.to_owned(),
            fragment: args.as_bytes().to_vec(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: vec![event] }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ];
    EventStream::new(stream::iter(events), || {})
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: Some(0.001),
    }
}

fn user_text(item: &ContextItem) -> Option<&str> {
    let ContextItem::User { parts } = item else {
        return None;
    };
    parts.iter().find_map(|part| match part {
        Part::Text { text } => Some(text.as_ref()),
        _ => None,
    })
}

fn process_fixture_path() -> PathBuf {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/dalgon/tests/fixtures/process");
    #[cfg(target_os = "linux")]
    let name = "setsid-grandchild.sh";
    #[cfg(target_os = "macos")]
    let name = "grandchild.sh";
    #[cfg(windows)]
    let name = "grandchild.ps1";
    root.join(name)
}

fn process_command(index: usize) -> String {
    let pid_file = format!("child-{index}.pid");
    #[cfg(windows)]
    {
        format!("powershell.exe -NoProfile -ExecutionPolicy Bypass -File grandchild.ps1 {pid_file}")
    }
    #[cfg(not(windows))]
    {
        format!(
            "./{} {pid_file}",
            process_fixture_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
        )
    }
}

fn full_extension(
    counts: &Arc<HookCounts>,
    reports: Arc<Mutex<Vec<AgentReport>>>,
    leaf_runs: &Arc<AtomicUsize>,
    idle_starts: Arc<AtomicUsize>,
) -> Result<Extension, Box<dyn Error + Send + Sync>> {
    let builder =
        ExtensionBuilder::new("gate-stress", "0.1.0", ServiceSet::from_names(["agents"])?)?
            .on_session_start_lossless(CountingHooks(Arc::clone(counts)))
            .on_session_end_lossless(CountingHooks(Arc::clone(counts)))
            .on_input(CountingHooks(Arc::clone(counts)))
            .on_before_turn(CountingHooks(Arc::clone(counts)))
            .on_before_request(CountingHooks(Arc::clone(counts)))
            .on_tool_call(CountingHooks(Arc::clone(counts)))
            .on_tool_result_lossless(CountingHooks(Arc::clone(counts)))
            .on_turn_end(CountingHooks(Arc::clone(counts)))
            .on_settled(CountingHooks(Arc::clone(counts)))
            .output_stream(Arc::new(CountingWatchFactory(Arc::clone(counts))))
            .model(ModelRecord {
                id: ModelId::parse(ROOT_MODEL)?,
                caps: caps(true),
                handler: Arc::new(ChildLoadModel { reports }),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(NESTED_MODEL)?,
                caps: caps(false),
                handler: Arc::new(NestedModel),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(MEMBER_MODEL)?,
                caps: caps(false),
                handler: Arc::new(MemberModel),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(LEAF_MODEL)?,
                caps: caps(false),
                handler: Arc::new(LeafModel {
                    runs: Arc::clone(leaf_runs),
                }),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(JOB_MODEL)?,
                caps: caps(true),
                handler: Arc::new(JobModel),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(IDLE_MODEL)?,
                caps: caps(false),
                handler: Arc::new(IdleModel {
                    starts: idle_starts,
                }),
                export: None,
            });
    Ok(builder.build()?)
}

fn shuttle_extension(
    counts: &Arc<HookCounts>,
    mail_results: Arc<Mutex<Vec<(String, dal_core::Receipt)>>>,
    completed_sends: Arc<AtomicUsize>,
    slow_starts: Arc<AtomicUsize>,
) -> Result<Extension, Box<dyn Error + Send + Sync>> {
    let builder =
        ExtensionBuilder::new("gate-stress", "0.1.0", ServiceSet::from_names(["agents"])?)?
            .on_session_start_lossless(ChildSessionHook(Arc::clone(counts)))
            .on_turn_end_lossless(CountingHooks(Arc::clone(counts)))
            .model(ModelRecord {
                id: ModelId::parse(SHUTTLE_ROOT_MODEL)?,
                caps: caps(false),
                handler: Arc::new(ShuttleRootModel {
                    mail_results,
                    completed_sends,
                }),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(SHUTTLE_SLOW_MODEL)?,
                caps: caps(false),
                handler: Arc::new(ShuttleSlowModel {
                    starts: slow_starts,
                }),
                export: None,
            });
    Ok(builder.build()?)
}

fn configured_product(
    data_root: &Path,
    user: &str,
    extension: Extension,
) -> Result<(Product, Config), Box<dyn Error + Send + Sync>> {
    let factory = dalgon::product();
    let config = Config::load(
        ConfigProduct::Dalgon,
        data_root,
        factory.defaults,
        Some(user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data_root.to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(extension);
    Ok((product, config))
}

fn nearest_rank_p99(samples: &[Duration]) -> Duration {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = ordered.len().saturating_mul(99).div_ceil(100);
    ordered[rank - 1]
}

async fn wait_turn_end(
    updates: &mut dal_agent::Subscription,
    turn: TurnId,
    stop: Stop,
) -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(delivery) = updates.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            if let dal_core::UpdateKind::TurnEnded {
                turn: ended,
                stop: actual,
            } = &update.kind
                && *ended == turn
            {
                assert_eq!(*actual, stop);
                return Ok::<(), TestError>(());
            }
        }
        Err(io::Error::other("session subscription closed before turn end").into())
    })
    .await?
}

async fn cancel_process_job(job: &mut ProcessJob) -> Result<Duration, TestError> {
    let started = tokio::time::Instant::now();
    let reply = job
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(job.turn),
        })
        .await?;
    assert!(matches!(reply, Reply::Done(_)));
    wait_turn_end(&mut job.updates, job.turn, Stop::Cancelled).await?;
    Ok(started.elapsed())
}

async fn cancel_jobs(jobs: &mut [ProcessJob], count: usize) -> Result<Vec<Duration>, TestError> {
    join_all(jobs.iter_mut().take(count).map(cancel_process_job))
        .await
        .into_iter()
        .collect()
}

async fn idle_cancellations(
    agent: &Agent,
    updates: &mut dal_agent::Subscription,
    starts: &AtomicUsize,
) -> Result<Vec<Duration>, TestError> {
    let mut samples = Vec::with_capacity(CANCELLATIONS);
    for _ in 0..CANCELLATIONS {
        let expected_start = starts.load(Ordering::Acquire) + 1;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "hold this idle-load turn".into(),
                }],
            })
            .await?;
        let Reply::Accepted { turn, .. } = reply else {
            return Err(io::Error::other("idle stress prompt was not accepted").into());
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            while starts.load(Ordering::Acquire) < expected_start {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await?;
        let started = tokio::time::Instant::now();
        let cancel = agent
            .submit(Command::Cancel {
                scope: CancelScope::Turn(turn),
            })
            .await?;
        assert!(matches!(cancel, Reply::Done(_)));
        wait_turn_end(updates, turn, Stop::Cancelled).await?;
        samples.push(started.elapsed());
    }
    Ok(samples)
}

async fn start_process_jobs(
    host: &Host,
    workspace: &Workspace,
) -> Result<Vec<ProcessJob>, TestError> {
    let mut jobs = Vec::with_capacity(PROCESS_JOBS);
    for index in 0..PROCESS_JOBS {
        let agent = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("stress-process"),
            )
            .await?;
        let model = agent
            .submit(Command::SetModel {
                model: synthetic_route(JOB_MODEL),
                save: Save::SessionOnly,
            })
            .await?;
        assert!(matches!(model, Reply::Done(_)));
        let view = agent.view(PageReq::default())?;
        let updates = agent.subscribe(Some((view.r#gen, view.seq)))?;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: format!("process-job-{index}").into(),
                }],
            })
            .await?;
        let Reply::Accepted { turn, .. } = reply else {
            return Err(io::Error::other("process stress prompt was not accepted").into());
        };
        jobs.push(ProcessJob {
            agent,
            updates,
            turn,
            pid_path: workspace.as_path().join(format!("child-{index}.pid")),
            pid: None,
        });
    }
    Ok(jobs)
}

fn read_pid(path: &Path) -> Result<Option<u32>, TestError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().parse()?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn wait_for_process_pids(jobs: &mut [ProcessJob]) -> Result<Vec<u32>, TestError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let mut ready = true;
        for job in jobs.iter_mut() {
            if job.pid.is_none() {
                job.pid = read_pid(&job.pid_path)?;
                ready &= job.pid.is_some();
            }
        }
        if ready {
            return jobs
                .iter()
                .map(|job| {
                    job.pid
                        .ok_or_else(|| io::Error::other("process pid was not recorded").into())
                })
                .collect();
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::other("not every process wrote its child pid").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn spawn_websocket_server(
    dir: &TestDir,
) -> Result<(ServerProcess, String, String), TestError> {
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let data_home = home.join(".local/share");
    let data_root = data_home.join("dal");
    fs::create_dir_all(home.join(".config/dal"))?;
    fs::create_dir_all(&workspace)?;
    fs::write(
        home.join(".config/dal/dal.toml"),
        "model = \"openai-responses/gpt-6\"\n",
    )?;
    let binary = support::dalgon_binary("dalgon")?;
    let token_output = ProcessCommand::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    if !token_output.status.success() {
        return Err(io::Error::other("could not create WebSocket test token").into());
    }
    let token = String::from_utf8(token_output.stdout)?.trim().to_owned();
    let child = ProcessCommand::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let server = ServerProcess(Some(child));
    let url = advertised_websocket(&data_root).await?;
    Ok((server, url, token))
}

async fn advertised_websocket(data_root: &Path) -> Result<String, TestError> {
    let dir = data_root.join("run/serve");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
                    && let Ok(bytes) = fs::read(entry.path())
                    && let Ok(value) = sonic_rs::from_slice::<sonic_rs::Value>(&bytes)
                    && let Some(url) = value.get("websocket").and_then(sonic_rs::Value::as_str)
                {
                    return Ok(url.to_owned());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::other("WebSocket advertisement did not appear").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn connect_websocket_clients(
    url: &str,
    token: &str,
) -> Result<
    Vec<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    TestError,
> {
    let mut clients = Vec::with_capacity(WEBSOCKET_CLIENTS);
    for index in 0..WEBSOCKET_CLIENTS {
        let mut request = url
            .into_client_request()
            .map_err(|error| io::Error::other(format!("WebSocket request rejected: {error}")))?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().map_err(
                |error: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    io::Error::other(format!("WebSocket token header rejected: {error}"))
                },
            )?,
        );
        let (mut socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(request),
        )
        .await??;
        let request =
            format!(r#"{{"jsonrpc":"2.0","id":{index},"method":"initialize","params":{{}}}}"#);
        socket.send(Message::Text(request.into())).await?;
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await?
            .ok_or_else(|| io::Error::other("WebSocket closed before initialize response"))??;
        let response: sonic_rs::Value = sonic_rs::from_str(&frame.into_text()?)?;
        assert_eq!(
            response.get("id").and_then(sonic_rs::Value::as_u64),
            Some(index as u64)
        );
        clients.push(socket);
    }
    Ok(clients)
}

fn resident_set_bytes() -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status")?;
        let value = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .and_then(|line| line.split_whitespace().nth(1))
            .ok_or_else(|| io::Error::other("VmRSS was not available"))?
            .parse::<u64>()
            .map_err(io::Error::other)?;
        Ok(value.saturating_mul(1024))
    }
    #[cfg(target_os = "macos")]
    {
        let output = ProcessCommand::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()?;
        let value = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(io::Error::other)?;
        Ok(value.saturating_mul(1024))
    }
    #[cfg(windows)]
    {
        let command = format!(
            "(Get-Process -Id {} | Select-Object -ExpandProperty WorkingSet64)",
            std::process::id()
        );
        let output = ProcessCommand::new("powershell.exe")
            .args(["-NoProfile", "-Command", &command])
            .output()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u64>()
            .map_err(io::Error::other)
    }
}

fn open_handle_count() -> io::Result<usize> {
    #[cfg(unix)]
    {
        Ok(fs::read_dir("/dev/fd")?.count())
    }
    #[cfg(windows)]
    {
        let command = format!(
            "(Get-Process -Id {} | Select-Object -ExpandProperty HandleCount)",
            std::process::id()
        );
        let output = ProcessCommand::new("powershell.exe")
            .args(["-NoProfile", "-Command", &command])
            .output()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<usize>()
            .map_err(io::Error::other)
    }
}

fn live_processes(pids: &[u32]) -> io::Result<Vec<u32>> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(target_os = "linux")]
    {
        let mut live = Vec::new();
        for pid in pids {
            let path = PathBuf::from(format!("/proc/{pid}/stat"));
            match fs::read_to_string(path) {
                Ok(stat) => {
                    let state = stat
                        .rsplit_once(')')
                        .and_then(|(_, rest)| rest.split_whitespace().next())
                        .ok_or_else(|| io::Error::other("process stat had no state"))?;
                    if state != "Z" && state != "X" {
                        live.push(*pid);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(live)
    }
    #[cfg(target_os = "macos")]
    {
        let list = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let output = ProcessCommand::new("ps")
            .args(["-o", "pid=", "-p", &list])
            .output()?;
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(|pid| pid.parse::<u32>().map_err(io::Error::other))
            .collect()
    }
    #[cfg(windows)]
    {
        let list = pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let command = format!(
            "$ids = @({list}); foreach ($id in $ids) {{ $p = Get-Process -Id $id -ErrorAction SilentlyContinue; if ($p) {{ $p.Id }} }}"
        );
        let output = ProcessCommand::new("powershell.exe")
            .args(["-NoProfile", "-Command", &command])
            .output()?;
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(|pid| pid.parse::<u32>().map_err(io::Error::other))
            .collect()
    }
}

async fn wait_for_processes_to_exit(pids: &[u32]) -> Result<(), TestError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let live = live_processes(pids)?;
        if live.is_empty() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "recorded child pids survived cancellation: {live:?}"
            ))
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[expect(clippy::too_many_lines, reason = "SC gate is one long stress scenario")]
async fn full_load_scenario() -> Result<(), TestError> {
    let data = TestDir::new()?;
    let workspace_dir = TestDir::new()?;
    let workspace = Workspace::new(workspace_dir.path().to_path_buf())?;
    let fixture_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
    // ProviderSet shares one ordered Script across every API route. Expand the
    // checked-in deterministic fixture for this >500-member load so one
    // member cannot exhaust the queue while its terminal stream is settling.
    let fixture_events = fs::read_to_string(&fixture_source)?;
    let fixture = data.path().join("stress-scripted-expanded.jsonl");
    let mut expanded_fixture = String::with_capacity(fixture_events.len() * 3);
    for _ in 0..3 {
        expanded_fixture.push_str(&fixture_events);
    }
    fs::write(&fixture, expanded_fixture)?;
    let reports = Arc::new(Mutex::new(Vec::with_capacity(CHILD_SESSIONS)));
    let hooks = Arc::new(HookCounts::default());
    let leaf_runs = Arc::new(AtomicUsize::new(0));
    let idle_starts = Arc::new(AtomicUsize::new(0));
    let extension = full_extension(
        &hooks,
        Arc::clone(&reports),
        &leaf_runs,
        Arc::clone(&idle_starts),
    )?;
    let user = format!(
        "model = \"{ROOT_MODEL}\"\napproval = \"all\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let (product, config) = configured_product(data.path(), &user, extension)?;
    let process_fixture = process_fixture_path();
    fs::copy(
        &process_fixture,
        workspace
            .as_path()
            .join(process_fixture.file_name().expect("fixture has a filename")),
    )?;
    #[cfg(unix)]
    fs::set_permissions(
        workspace
            .as_path()
            .join(process_fixture.file_name().expect("fixture has a filename")),
        fs::Permissions::from_mode(0o755),
    )?;
    let pre_run_handles = open_handle_count()?;
    let env = Env {
        vars: BTreeMap::default(),
        cwd: workspace.as_path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let root = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("stress-root"),
        )
        .await?;
    let root_id = root.view(PageReq::default())?.session.id;
    let root_updates = root.subscribe(None)?;
    let root_turn = root
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the full nested synthetic child probe".into(),
            }],
        })
        .await?;
    let Reply::Accepted {
        turn: _root_turn, ..
    } = root_turn
    else {
        return Err(io::Error::other("child-load prompt was not accepted").into());
    };
    wait_children_complete(&root, &reports).await?;
    assert_eq!(locked(&reports).len(), CHILD_SESSIONS);
    assert_eq!(leaf_runs.load(Ordering::Acquire), 4);
    assert!(assistant_text(&root.view(PageReq::default())?).contains("children complete"));
    drop(root_updates);
    let current = root.view(PageReq::default())?;
    let mut idle_updates = root.subscribe(Some((current.r#gen, current.seq)))?;
    root.submit(Command::SetModel {
        model: synthetic_route(IDLE_MODEL),
        save: Save::SessionOnly,
    })
    .await?;
    let idle_samples = idle_cancellations(&root, &mut idle_updates, &idle_starts).await?;
    let idle_p99 = nearest_rank_p99(&idle_samples);
    // Wall-clock and resource bounds assert only on the nightly
    // idle-machine lane; shared CI runners cannot hold them.
    let budgets = std::env::var_os("DAL_TIMING_BUDGETS").is_some();
    if budgets {
        assert!(idle_p99 < CANCEL_P99, "idle cancel p99 was {idle_p99:?}");
    }
    let mut process_jobs = start_process_jobs(&host, &workspace).await?;
    let pids = wait_for_process_pids(&mut process_jobs).await?;
    let web_dir = TestDir::new()?;
    let (server, websocket_url, token) = spawn_websocket_server(&web_dir).await?;
    let web_sockets = connect_websocket_clients(&websocket_url, &token).await?;
    let rss = resident_set_bytes()?;
    let handles = open_handle_count()?;
    if budgets {
        assert!(
            rss < RESOURCE_LIMIT_BYTES,
            "resident memory was {rss} bytes"
        );
        assert!(handles < HANDLE_LIMIT, "open handle count was {handles}");
    }
    let loaded_samples = cancel_jobs(&mut process_jobs, CANCELLATIONS).await?;
    let loaded_p99 = nearest_rank_p99(&loaded_samples);
    if budgets {
        assert!(
            loaded_p99 < CANCEL_P99,
            "full-load cancel p99 was {loaded_p99:?}"
        );
    }
    wait_for_processes_to_exit(&pids[..CANCELLATIONS]).await?;
    let remaining_samples = cancel_jobs(
        &mut process_jobs[CANCELLATIONS..],
        PROCESS_JOBS - CANCELLATIONS,
    )
    .await?;
    assert_eq!(remaining_samples.len(), PROCESS_JOBS - CANCELLATIONS);
    wait_for_processes_to_exit(&pids).await?;
    let expected_root_turns = idle_samples.len() + 1;
    drop(web_sockets);
    drop(server);
    let report = host.shutdown(Duration::from_secs(30)).await;
    // The root's session-end sweep cascade-closes the children before the
    // shutdown loop reaches them, so `sessions_closed` only ever counts the
    // top-level sessions plus the children the loop got to first — every
    // session ending exactly once is asserted by `session_ends` below.
    assert!(report.sessions_closed > PROCESS_JOBS);
    assert_eq!(report.tasks_remaining, 0);
    assert_eq!(
        hooks.session_starts.load(Ordering::Acquire),
        CHILD_SESSIONS + PROCESS_JOBS + 1
    );
    assert_eq!(
        hooks.session_ends.load(Ordering::Acquire),
        CHILD_SESSIONS + PROCESS_JOBS + 1
    );
    assert!(hooks.inputs.load(Ordering::Acquire) > 0);
    assert!(hooks.before_turns.load(Ordering::Acquire) > 0);
    assert!(hooks.before_requests.load(Ordering::Acquire) > 0);
    assert!(hooks.tool_calls.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.tool_results.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.turn_ends.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.settled.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.stream_starts.load(Ordering::Acquire) > 0);
    assert!(hooks.stream_deltas.load(Ordering::Acquire) > 0);
    assert!(hooks.stream_finishes.load(Ordering::Acquire) > 0);
    let session_ids = expected_session_ids(root_id, &process_jobs, &reports, &hooks);
    assert_eq!(session_ids.len(), CHILD_SESSIONS + PROCESS_JOBS + 1);
    {
        let ends_by_session = locked(&hooks.ends_by_session);
        assert_eq!(ends_by_session.len(), session_ids.len());
        assert!(
            session_ids
                .iter()
                .all(|id| ends_by_session.get(id) == Some(&1))
        );
    }
    verify_journal_ends(
        data.path(),
        &workspace,
        &session_ids,
        root_id,
        expected_root_turns,
    )
    .await?;
    drop(process_jobs);
    drop(idle_updates);
    drop(root);
    let after_handles = open_handle_count()?;
    assert_eq!(
        after_handles, pre_run_handles,
        "open handles did not return to baseline"
    );
    Ok(())
}

async fn wait_children_complete(
    root: &Agent,
    reports: &Mutex<Vec<AgentReport>>,
) -> Result<(), TestError> {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if locked(reports).len() == CHILD_SESSIONS {
                let view = root.view(PageReq::default())?;
                if matches!(view.turn, TurnState::Idle)
                    && assistant_text(&view).contains("children complete")
                {
                    return Ok::<(), TestError>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

fn assistant_text(view: &dal_core::View) -> String {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            dal_core::Block::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect()
}

fn expected_session_ids(
    root_id: SessionId,
    jobs: &[ProcessJob],
    reports: &Mutex<Vec<AgentReport>>,
    hooks: &HookCounts,
) -> Vec<SessionId> {
    let mut ids = vec![root_id];
    ids.extend(jobs.iter().map(|job| {
        job.agent
            .view(PageReq::default())
            .expect("job session view")
            .session
            .id
    }));
    ids.extend(locked(reports).iter().map(|report| report.session));
    let child_hook_ids = locked(&hooks.children_by_session);
    assert_eq!(child_hook_ids.len(), CHILD_SESSIONS);
    ids
}

async fn verify_journal_ends(
    data_root: &Path,
    workspace: &Workspace,
    session_ids: &[SessionId],
    root_id: SessionId,
    root_turns: usize,
) -> Result<(), TestError> {
    let store = Store::new(
        data_root.to_path_buf(),
        workspace.clone(),
        StoreProduct::Dal,
    );
    for id in session_ids {
        let (mut journal, _) = store.open_session(*id).await?;
        let ends = journal
            .records()
            .iter()
            .filter(|record| matches!(record, Record::TurnEnd { .. }))
            .count();
        let expected = if *id == root_id { root_turns } else { 1 };
        assert_eq!(ends, expected, "session {id} had {ends} turn-end records");
        journal.close().await?;
    }
    Ok(())
}

async fn full_setup_for_actor_smoke(
    data_root: &Path,
    extension: Extension,
) -> Result<(Host, Agent, Workspace), TestError> {
    let workspace_dir = data_root.join("workspace");
    let workspace = Workspace::new(workspace_dir.clone())?;
    let user = format!("model = \"{SHUTTLE_SLOW_MODEL}\"\n");
    let (product, config) = configured_product(data_root, &user, extension)?;
    let host = Host::start(
        product,
        config,
        Env {
            vars: BTreeMap::default(),
            cwd: workspace_dir,
            sandbox_helper: None,
        },
    )
    .await?;
    let parent = host
        .open(
            SessionRef::Ephemeral {
                workspace: workspace.clone(),
            },
            ClientId::new("shuttle-parent"),
        )
        .await?;
    parent
        .submit(Command::SetModel {
            model: synthetic_route(SHUTTLE_ROOT_MODEL),
            save: Save::SessionOnly,
        })
        .await?;
    Ok((host, parent, workspace))
}

fn append_effect_records(out: &[Effect], records: &mut Vec<Record>) {
    for effect in out {
        if let Effect::Emit(emit) = effect {
            records.extend(emit.records.iter().cloned());
        }
    }
}

fn stamp() -> dal_core::Timestamp {
    dal_core::Timestamp::UNIX_EPOCH
}

fn replay_matches_model(
    records: &[Record],
    model: &PhaseModel,
    turns_allocated: u64,
    turns_completed: usize,
) -> Result<(), String> {
    let (replayed, recovery_effects) = Session::replay(records.iter().cloned(), stamp())
        .map_err(|error| format!("journal replay failed: {error}"))?;
    let completed = u64::try_from(turns_completed)
        .map_err(|error| format!("completed-turn count is not representable: {error}"))?;
    let terminal_records = |source: &[Record]| {
        source
            .iter()
            .filter_map(|record| match record {
                Record::TurnEnd { turn, stop, .. } => Some((*turn, stop.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let expected_completed: Vec<_> = (1..=completed)
        .filter_map(|number| {
            std::num::NonZeroU64::new(number)
                .map(|number| (TurnId::new(number), dal_core::TurnEndStop::Cancelled))
        })
        .collect();
    let actual_completed = terminal_records(records);
    if actual_completed != expected_completed {
        return Err(format!(
            "completed journal ends differ: expected {expected_completed:?}, got {actual_completed:?}"
        ));
    }

    let active_turns = usize::from(!matches!(model, PhaseModel::Idle));
    if turns_completed.checked_add(active_turns) != usize::try_from(turns_allocated).ok() {
        return Err(format!(
            "model counts disagree: allocated={turns_allocated}, completed={turns_completed}, active={active_turns}"
        ));
    }
    // Prompt allocates an Opening turn but does not journal TurnStart until Guard.
    // Replay therefore repairs only completed Running turns, not an Opening turn.
    let replay_started = match model {
        PhaseModel::Idle | PhaseModel::Opening(_) => completed,
        PhaseModel::Running(_) => turns_allocated,
    };
    let mut recovered_records = records.to_vec();
    append_effect_records(&recovery_effects, &mut recovered_records);
    let expected_recovered: Vec<_> = (1..=replay_started)
        .filter_map(|number| {
            std::num::NonZeroU64::new(number).map(|number| {
                let stop = if number.get() <= completed {
                    dal_core::TurnEndStop::Cancelled
                } else {
                    dal_core::TurnEndStop::Aborted
                };
                (TurnId::new(number), stop)
            })
        })
        .collect();
    let actual_recovered = terminal_records(&recovered_records);
    if actual_recovered != expected_recovered {
        return Err(format!(
            "recovered journal ends differ: expected {expected_recovered:?}, got {actual_recovered:?}"
        ));
    }
    if !matches!(
        (model, replayed.phase()),
        (
            PhaseModel::Idle | PhaseModel::Opening(_) | PhaseModel::Running(_),
            Phase::Idle
        )
    ) {
        return Err(format!(
            "replay phase {:?} disagrees with recovery of model phase",
            replayed.phase()
        ));
    }
    Ok(())
}

fn assert_live_phase(session: &Session, expected: &PhaseModel) -> bool {
    match (expected, session.phase()) {
        (PhaseModel::Idle, Phase::Idle) => true,
        (PhaseModel::Opening(expected), Phase::Opening { turn, .. })
        | (PhaseModel::Running(expected), Phase::Running { turn, .. }) => expected == turn,
        _ => false,
    }
}

enum PhaseModel {
    Idle,
    Opening(TurnId),
    Running(TurnId),
}

enum GeneratedStep {
    Prompt(TurnId),
    Guard(TurnId),
    Cancel(TurnId),
    Reject,
}

#[tokio::test]
async fn stress_500_children_200_jobs_nested_scopes_synthetic_models_and_websockets()
-> Result<(), TestError> {
    let _serial = TEST_LOCK.lock().await;
    full_load_scenario().await
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn stress_session_step_matches_property_model(actions in prop::collection::vec(any::<u8>(), 1..40)) {
        let _serial = TEST_LOCK.blocking_lock();
        let mut session = Session::replay([], stamp()).expect("empty fold session").0;
        let mut model = PhaseModel::Idle;
        let mut records = Vec::new();
        let mut out = Vec::new();
        let mut next_turn = 1_u64;
        let mut turns_completed = 0_usize;

        for action in [0_u8, 1, 2].into_iter().chain(actions) {
            let stale_turn = TurnId::new(
                std::num::NonZeroU64::new(10_000).expect("nonzero stale test turn"),
            );
            let step = match (action % 4, &model) {
                (0, PhaseModel::Idle) => GeneratedStep::Prompt(
                    TurnId::new(
                        std::num::NonZeroU64::new(next_turn).expect("nonzero model turn"),
                    ),
                ),
                (1, PhaseModel::Opening(turn)) => GeneratedStep::Guard(*turn),
                (2, PhaseModel::Running(turn)) => GeneratedStep::Cancel(*turn),
                _ => GeneratedStep::Reject,
            };
            let event = match &step {
                GeneratedStep::Prompt(_) => Event::Command {
                    cmd: Command::Prompt {
                        expect: Expect::Idle,
                        content: vec![Part::Text {
                            text: format!("question-{action}").into(),
                        }],
                    },
                    by: ClientId::new("property"),
                },
                GeneratedStep::Guard(turn) => Event::Guard {
                    turn: *turn,
                    call: None,
                    extension: None,
                    outcome: HookOutcome::new(
                        HookEvent::BeforeTurn,
                        HookVerdict::BeforeTurn(None),
                    ).expect("before-turn verdict matches"),
                },
                GeneratedStep::Cancel(turn) => Event::Cancel {
                    scope: CancelScope::Turn(*turn),
                    partial: None,
                },
                GeneratedStep::Reject => Event::Command {
                    cmd: Command::Steer {
                        turn: stale_turn,
                        content: Vec::new(),
                    },
                    by: ClientId::new("property"),
                },
            };

            let before_session = session.clone();
            let previous_output = vec![Effect::Reply(Ok(Reply::Done(dal_core::Output::Nothing)))];
            out.clone_from(&previous_output);
            let result = session.step(event, stamp(), &mut out);
            match step {
                GeneratedStep::Prompt(turn) => {
                    prop_assert!(result.is_ok(), "model prompt is accepted while idle");
                    model = PhaseModel::Opening(turn);
                    next_turn += 1;
                    append_effect_records(&out, &mut records);
                }
                GeneratedStep::Guard(turn) => {
                    prop_assert!(result.is_ok(), "matching guard opens the running phase");
                    model = PhaseModel::Running(turn);
                    append_effect_records(&out, &mut records);
                }
                GeneratedStep::Cancel(_) => {
                    prop_assert!(result.is_ok(), "active-turn cancellation is accepted");
                    model = PhaseModel::Idle;
                    turns_completed += 1;
                    append_effect_records(&out, &mut records);
                }
                GeneratedStep::Reject => {
                    prop_assert!(result.is_err(), "stale steering is rejected");
                    prop_assert_eq!(&session, &before_session, "rejection preserves fold state");
                    prop_assert_eq!(&out, &previous_output, "rejection preserves effects");
                }
            }

            prop_assert!(assert_live_phase(&session, &model), "accepted fold state matches the model");
            let replay_check = replay_matches_model(&records, &model, next_turn - 1, turns_completed);
            prop_assert!(replay_check.is_ok(), "{replay_check:?}");
        }
    }
}

#[test]
fn stress_shuttle_schedules_preserve_actor_invariants() {
    const DEFAULT_SHUTTLE_SEED: u64 = 0x5eed_01a7_0c70_5e5d;
    let _serial = TEST_LOCK.blocking_lock();
    // Keep one real actor smoke run beside the pure scheduler run. The smoke
    // run proves the public actor path; the scheduler run is deliberately
    // limited to Shuttle-aware futures and the in-memory journal.
    let seed = std::env::var("SHUTTLE_RANDOM_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SHUTTLE_SEED);
    let data = TestDir::new().expect("actor smoke data directory");
    let data_root = data.path().to_path_buf();
    fs::create_dir_all(data_root.join("workspace")).expect("actor smoke workspace directory");
    actor_schedule(&data_root);

    // Shuttle controls every task poll in this half. No Tokio runtime, file
    // shard, process, or network handle crosses the Shuttle continuation.
    shuttle::check_random_with_seed(shuttle_actor_schedule, seed, 32);
}

/// Public-API smoke for the actual actor. This intentionally runs outside
/// Shuttle: the actor owns Tokio tasks, while the schedule proof below uses
/// the same fold/journal protocol on Shuttle's executor.
#[expect(
    clippy::panic,
    reason = "SC test aborts on impossible scheduling results"
)]
fn actor_schedule(data_root: &Path) {
    let hooks = Arc::new(HookCounts::default());
    let mail_results = Arc::new(Mutex::new(Vec::new()));
    let completed_sends = Arc::new(AtomicUsize::new(0));
    let slow_starts = Arc::new(AtomicUsize::new(0));
    let extension = shuttle_extension(
        &hooks,
        Arc::clone(&mail_results),
        Arc::clone(&completed_sends),
        Arc::clone(&slow_starts),
    )
    .expect("actor smoke extension builds");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("actor smoke runtime builds");
    let (host, parent, workspace) = runtime
        .block_on(full_setup_for_actor_smoke(data_root, extension))
        .expect("actor smoke opens");
    let parent_id = parent
        .view(PageReq::default())
        .expect("parent view")
        .session
        .id;
    let prompt = runtime
        .block_on(parent.submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "start cancellable mailbox stress".into(),
            }],
        }))
        .expect("parent turn starts");
    let Reply::Accepted { turn, .. } = prompt else {
        panic!("actor smoke prompt was not accepted");
    };
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while slow_starts.load(Ordering::Acquire) == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("slow child did not arrive");
    });

    let (cancel_result, shutdown_report) = runtime.block_on(async {
        let cancel = async {
            tokio::task::yield_now().await;
            parent
                .submit(Command::Cancel {
                    scope: CancelScope::Turn(turn),
                })
                .await
        };
        let shutdown = async {
            tokio::task::yield_now().await;
            host.clone().shutdown(Duration::from_secs(10)).await
        };
        tokio::join!(cancel, shutdown)
    });
    let _cancel_accepted = match cancel_result {
        Ok(Reply::Done(_)) => true,
        Err(dal_agent::AgentError::SessionClosed { id }) if id == parent_id => false,
        other => panic!("cancel/shutdown race returned an unexpected result: {other:?}"),
    };
    assert_eq!(
        shutdown_report.tasks_remaining, 0,
        "shutdown leaked tracked tasks"
    );
    let final_report = runtime.block_on(host.shutdown(Duration::from_secs(10)));
    assert_eq!(
        final_report.tasks_remaining, 0,
        "repeated shutdown leaked tracked tasks"
    );

    drop(runtime);
    drop(workspace);
}

struct ShuttleActorState {
    session: Session,
    journal: StoreJournal,
    child_journal: StoreJournal,
    turn: TurnId,
    parent: SessionId,
    child: SessionId,
    turn_open: bool,
    shutting_down: bool,
    accepted: Vec<(String, dal_core::Receipt)>,
    tracked_tasks: usize,
}

type StoreJournal = dal_store::Journal;
type ShuttleActor = Arc<ShuttleMutex<ShuttleActorState>>;

fn shuttle_actor_schedule() {
    // The futures below deliberately overlap all four gate races: turn
    // cancellation with a journal receipt, mailbox delivery with shutdown,
    // two terminal-end attempts, and tracked-task drain at shutdown. Every
    // shared access uses ShuttleMutex, so sampled Shuttle schedules exercise
    // serialized access while the assertions below check durable behavior;
    // this is not an exhaustive race-freedom proof.
    let actor = Arc::new(ShuttleMutex::new(shuttle_actor_state()));
    shuttle_record_mail(&actor, "mail-before-race");

    shuttle_future::block_on(async {
        let receipt_race =
            shuttle_future::spawn(shuttle_mail_task(Arc::clone(&actor), "mail-cancel-race"));
        let cancel_race = shuttle_future::spawn(shuttle_cancel_task(Arc::clone(&actor)));
        let shutdown_race = shuttle_future::spawn(shuttle_shutdown_task(Arc::clone(&actor)));
        let delivery_race =
            shuttle_future::spawn(shuttle_mail_task(Arc::clone(&actor), "mail-shutdown-race"));
        let double_end_a = shuttle_future::spawn(shuttle_cancel_task(Arc::clone(&actor)));
        let double_end_b = shuttle_future::spawn(shuttle_cancel_task(Arc::clone(&actor)));
        let tracked_a = shuttle_future::spawn(shuttle_tracked_task(Arc::clone(&actor)));
        let tracked_b = shuttle_future::spawn(shuttle_tracked_task(Arc::clone(&actor)));
        let _ = futures::join!(
            receipt_race,
            cancel_race,
            shutdown_race,
            delivery_race,
            double_end_a,
            double_end_b,
            tracked_a,
            tracked_b,
        );
    });

    let mut actor = actor.lock().expect("Shuttle actor state lock");
    let mail_records: Vec<_> = actor
        .child_journal
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::Mail(mail) => Some(mail.text.to_string()),
            _ => None,
        })
        .collect();
    let accepted: HashSet<_> = actor
        .accepted
        .iter()
        .map(|(text, _)| text.clone())
        .collect();
    assert!(
        accepted.iter().all(|text| mail_records.contains(text)),
        "accepted mail was lost"
    );
    assert_eq!(
        accepted.len(),
        actor.accepted.len(),
        "mail receipt was duplicated"
    );
    assert!(actor.accepted.iter().all(|(_, receipt)| {
        matches!(
            receipt,
            dal_core::Receipt::Delivered | dal_core::Receipt::Woken | dal_core::Receipt::Buffered
        )
    }));
    let turn_ends: Vec<_> = actor
        .journal
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::TurnEnd { turn, .. } if *turn == actor.turn => Some(*turn),
            _ => None,
        })
        .collect();
    assert_eq!(
        turn_ends.len(),
        1,
        "the actor wrote a double turn-end record"
    );
    assert_eq!(
        actor.tracked_tasks, 0,
        "SessionTasks leaked at actor shutdown"
    );
    assert!(actor.shutting_down, "shutdown race did not close the actor");
    assert_eq!(
        actor.parent,
        actor.journal.id(),
        "parent journal identity changed"
    );
    assert_eq!(
        actor.child,
        actor.child_journal.id(),
        "child journal identity changed"
    );
    assert_ne!(
        actor.parent, actor.child,
        "mailbox endpoints must be distinct"
    );
    shuttle_future::block_on(actor.journal.close()).expect("Shuttle parent journal closes");
    shuttle_future::block_on(actor.child_journal.close()).expect("Shuttle child journal closes");
}

fn shuttle_actor_state() -> ShuttleActorState {
    let workspace = Workspace::new(PathBuf::from("/shuttle-workspace")).expect("Shuttle workspace");
    let store = Store::new(PathBuf::from("/shuttle-data"), workspace, StoreProduct::Dal);
    let parent = SessionId::new_v7();
    let child = SessionId::new_v7();
    let mut journal = store.ephemeral_session(parent);
    let child_journal = store.ephemeral_session(child);
    let (mut session, _recovery) = Session::replay([], stamp()).expect("Shuttle fold session");
    let turn = TurnId::new(std::num::NonZeroU64::new(1).expect("Shuttle turn id"));
    let mut effects = Vec::new();
    session
        .step(
            Event::Command {
                cmd: Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text {
                        text: "shuttle prompt".into(),
                    }],
                },
                by: ClientId::new("shuttle"),
            },
            stamp(),
            &mut effects,
        )
        .expect("Shuttle prompt fold");
    let mut records = Vec::new();
    append_effect_records(&effects, &mut records);
    effects.clear();
    session
        .step(
            Event::Guard {
                turn,
                call: None,
                extension: None,
                outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
                    .expect("Shuttle guard verdict"),
            },
            stamp(),
            &mut effects,
        )
        .expect("Shuttle guard fold");
    append_effect_records(&effects, &mut records);
    shuttle_future::block_on(journal.append(records)).expect("Shuttle journal bootstrap");
    ShuttleActorState {
        session,
        journal,
        child_journal,
        turn,
        parent,
        child,
        turn_open: true,
        shutting_down: false,
        accepted: Vec::new(),
        tracked_tasks: 2,
    }
}

async fn shuttle_mail_task(actor: ShuttleActor, text: &'static str) {
    shuttle_future::yield_now().await;
    shuttle_record_mail(&actor, text);
}

fn shuttle_record_mail(actor: &ShuttleActor, text: &str) {
    let mut actor = actor.lock().expect("Shuttle actor state lock");
    if actor.shutting_down {
        return;
    }
    let record = Record::Mail(dal_core::Mail {
        at: stamp(),
        from: actor.parent,
        to: actor.child,
        mode: dal_core::MailMode::Aside,
        text: text.into(),
        reply_to: None,
    });
    shuttle_future::block_on(actor.child_journal.append(vec![record]))
        .expect("Shuttle mailbox journal append");
    actor
        .accepted
        .push((text.to_owned(), dal_core::Receipt::Delivered));
}

async fn shuttle_cancel_task(actor: ShuttleActor) {
    shuttle_future::yield_now().await;
    let mut actor = actor.lock().expect("Shuttle actor state lock");
    if !actor.turn_open {
        return;
    }
    let mut effects = Vec::new();
    let turn = actor.turn;
    let result = actor.session.step(
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: None,
        },
        stamp(),
        &mut effects,
    );
    if result.is_ok() {
        let mut records = Vec::new();
        append_effect_records(&effects, &mut records);
        shuttle_future::block_on(actor.journal.append(records))
            .expect("Shuttle cancellation journal append");
        actor.turn_open = false;
    }
}

async fn shuttle_shutdown_task(actor: ShuttleActor) {
    shuttle_future::yield_now().await;
    let mut actor = actor.lock().expect("Shuttle actor state lock");
    actor.shutting_down = true;
    if actor.turn_open {
        let mut effects = Vec::new();
        let turn = actor.turn;
        let result = actor.session.step(
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
            stamp(),
            &mut effects,
        );
        if result.is_ok() {
            let mut records = Vec::new();
            append_effect_records(&effects, &mut records);
            shuttle_future::block_on(actor.journal.append(records))
                .expect("Shuttle shutdown journal append");
            actor.turn_open = false;
        }
    }
}

async fn shuttle_tracked_task(actor: ShuttleActor) {
    shuttle_future::yield_now().await;
    let mut actor = actor.lock().expect("Shuttle actor state lock");
    actor.tracked_tasks = actor.tracked_tasks.saturating_sub(1);
}
