#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "SC test"))]
#![expect(clippy::expect_used, reason = "SC test")]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test exercises real process and filesystem boundaries"
)]

//! Full-load session lifecycle: hooks, phases, actor scheduling, and shutdown races.
//!
//! The load scenario also drives the real `dalgon` TUI on a pseudoterminal,
//! attached over a loopback WebSocket to the host that carries the 500-child
//! and 200-job load, and measures keypress echo and cancel latency there.
#[cfg(unix)]
#[expect(
    dead_code,
    reason = "PTY support includes helpers used by other gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
#[expect(
    dead_code,
    reason = "gate helpers are shared across integration targets"
)]
mod support;

use std::{
    collections::HashMap,
    error::Error,
    fs::{self, File},
    io,
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
#[cfg(not(all(windows, target_arch = "aarch64")))]
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
/// One fast exec call that settles while its turn stays live, used only by
/// the result-observer probe: the scripted command prints a deterministic
/// marker and exits 0 at once, so the call never approaches a cancel.
const PROBE_MODEL: &str = "gate-stress/probe";
/// The deterministic probe command marker echoed by the probe session's
/// completed call and readable in its journaled tool result.
const PROBE_MARKER: &str = "gate-stress-probe-marker-7f3a";
/// The probe session's tool call identity, matched exactly in the observer
/// recording and in the probe session's journal.
const PROBE_CALL: &str = "probe-call";
const TERMINAL_MODEL: &str = "gate-stress/terminal";
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
    /// Delivered result observers, one `(session, call)` entry per delivery.
    observed_result_calls: Mutex<Vec<(SessionId, CallId)>>,
    /// Admitted tool calls, one `(session, call)` entry per dispatch, in
    /// dispatch order. The admission hook runs before the tool body, so every
    /// started call appears here even when its turn is cancelled later.
    admitted_calls: Mutex<Vec<(SessionId, CallId)>>,
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
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        self.0.tool_calls.fetch_add(1, Ordering::Relaxed);
        locked(&self.0.admitted_calls).push((cx.session, event.call));
        Box::pin(async { Ok(ToolCallVerdict::Allow) })
    }
}

impl ObserveHook<ToolResultEvent> for CountingHooks {
    fn call(
        &self,
        event: ToolResultEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        self.0.tool_results.fetch_add(1, Ordering::Relaxed);
        locked(&self.0.observed_result_calls).push((cx.session, event.call));
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
                    name: Some(format!("member-{index}").into()),
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

/// One fast exec call that settles while its turn stays live, used only by
/// the result-observer probe: the command prints `PROBE_MARKER` and exits 0.
struct ProbeModel;

impl ModelHandler for ProbeModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if request.context.iter().any(|item| {
                matches!(item, ContextItem::ToolResult { name, .. } if name.as_ref() == "exec")
            }) {
                return Ok(text_stream("probe complete"));
            }
            let args = format!(
                r#"{{"command":"printf '%s' {PROBE_MARKER}","timeout_seconds":60,"foreground_s":30}}"#
            );
            Ok(tool_call_stream("probe-call", &args))
        })
    }
}

/// Runs one exec job per terminal prompt. `JobModel` takes the first prompt
/// in the context and answers at once once any exec result exists, so it
/// serves one turn per session. This model reads the newest prompt and
/// answers only a result that ends the context, so one long-lived terminal
/// session can run many turns.
struct TerminalJobModel;

impl ModelHandler for TerminalJobModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if matches!(
                request.context.last(),
                Some(ContextItem::ToolResult { name, .. }) if name.as_ref() == "exec"
            ) {
                return Ok(text_stream("process started"));
            }
            let Some(index) = request
                .context
                .iter()
                .rev()
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
                name: Some("shuttle-child".into()),
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

/// Sessions the real terminal clients attached to the loaded host; empty
/// where no pseudoterminal exists.
#[derive(Default)]
struct TuiSessions {
    /// The session that only echoed typed keys. It never held a turn.
    echo: Option<SessionId>,
    /// The session whose terminal cancelled this many running exec turns.
    cancel: Option<(SessionId, usize)>,
}

impl TuiSessions {
    fn count(&self) -> usize {
        usize::from(self.echo.is_some()) + usize::from(self.cancel.is_some())
    }
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
        premium_requests: None,
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/dalgon/tests/fixtures/process");
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
                id: ModelId::parse(PROBE_MODEL)?,
                caps: caps(true),
                handler: Arc::new(ProbeModel),
                export: None,
            })
            .model(ModelRecord {
                id: ModelId::parse(TERMINAL_MODEL)?,
                // One session runs every terminal turn, so give it a window
                // that 100 turns cannot fill.
                caps: Caps {
                    context_window: Some(1_000_000),
                    ..caps(true)
                },
                handler: Arc::new(TerminalJobModel),
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

/// Probe proof: the live call delivered exactly one result observer, and its
/// journal holds exactly one matching success result.
fn assert_tool_result_delivery(hooks: &HookCounts, probe: &ProbeSession) -> Result<(), TestError> {
    let observed = locked(&hooks.observed_result_calls);
    let probe_calls = observed
        .iter()
        .filter(|(session, call)| *session == probe.session && call.as_str() == PROBE_CALL)
        .count();
    if probe_calls != 1 {
        return Err(io::Error::other(format!(
            "the live probe call delivered {probe_calls} result observers, expected exactly 1",
        ))
        .into());
    }
    drop(observed);
    let probe_view = probe.agent.view(PageReq::default())?;
    let mut probe_results = Vec::new();
    for entry in &probe_view.entries.items {
        if let EntryKind::ToolResult {
            call,
            name,
            error,
            parts,
            ..
        } = &entry.kind
            && call.as_str() == PROBE_CALL
        {
            probe_results.push((name.clone(), *error, text_of_parts(parts)));
        }
    }
    if probe_results.len() != 1 {
        return Err(io::Error::other(format!(
            "probe session {} holds {} results for {PROBE_CALL}, expected 1",
            probe.session,
            probe_results.len(),
        ))
        .into());
    }
    let (name, error, text) = &probe_results[0];
    if name.as_ref() != "exec" || *error || !text.contains(PROBE_MARKER) {
        return Err(io::Error::other(format!(
            "probe session {} journaled the wrong result for {PROBE_CALL}: name={name} error={error} text={text:?}",
            probe.session,
        ))
        .into());
    }
    Ok(())
}

/// Cancelled-call proof per `(session, call)`: the admitted id comes from the
/// admission recording, never from the result being checked. Each cancelled
/// session must admit exactly one call; its journal must hold exactly one
/// matching interrupted result; the observer recording must hold zero
/// entries for that pair.
fn assert_cancelled_job_journals(jobs: &[ProcessJob], hooks: &HookCounts) -> Result<(), TestError> {
    let observed = locked(&hooks.observed_result_calls).clone();
    let admitted = locked(&hooks.admitted_calls).clone();
    for job in jobs {
        let view = job.agent.view(PageReq::default())?;
        let session = view.session.id;
        let admitted_calls: Vec<CallId> = admitted
            .iter()
            .filter(|(admitted_session, _)| *admitted_session == session)
            .map(|(_, call)| call.clone())
            .collect();
        if admitted_calls.len() != 1 {
            return Err(io::Error::other(format!(
                "cancelled session {session} admitted {} calls, expected exactly 1",
                admitted_calls.len(),
            ))
            .into());
        }
        let admitted_call = &admitted_calls[0];
        let mut matches = Vec::new();
        for entry in &view.entries.items {
            if let EntryKind::ToolResult {
                call,
                name,
                error,
                parts,
                ..
            } = &entry.kind
                && call == admitted_call
            {
                matches.push((name.clone(), *error, text_of_parts(parts)));
            }
        }
        if matches.len() != 1 {
            return Err(io::Error::other(format!(
                "cancelled session {session} holds {} results for admitted call {admitted_call}, expected exactly 1",
                matches.len(),
            ))
            .into());
        }
        let (name, error, text) = &matches[0];
        if name.as_ref() != "exec" || !error || text.as_str() != "Tool call interrupted by user." {
            return Err(io::Error::other(format!(
                "cancelled session {session} journaled the wrong interrupted result for {admitted_call}: name={name} error={error} text={text:?}",
            ))
            .into());
        }
        let seen = observed
            .iter()
            .filter(|(observed_session, observed_call)| {
                *observed_session == session && *observed_call == *admitted_call
            })
            .count();
        if seen != 0 {
            return Err(io::Error::other(format!(
                "cancelled pair ({session}, {admitted_call}) delivered a result observer"
            ))
            .into());
        }
    }
    drop(admitted);
    drop(observed);
    Ok(())
}

fn text_of_parts(parts: &[dal_core::JournalPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            dal_core::JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<&str>>()
        .join("")
}

/// The live result-observer probe session: one turn whose single exec call
/// completes while the turn stays open, so the terminal result observer
/// fires on a live token by contract. Cancel jobs never touch this session.
struct ProbeSession {
    agent: Agent,
    session: SessionId,
}

async fn start_probe_session(
    host: &Host,
    workspace: &Workspace,
) -> Result<ProbeSession, TestError> {
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("stress-probe"),
        )
        .await?;
    let model = agent
        .submit(Command::SetModel {
            model: synthetic_route(PROBE_MODEL),
            save: Save::SessionOnly,
        })
        .await?;
    assert!(matches!(model, Reply::Done(_)));
    let view = agent.view(PageReq::default())?;
    let session = view.session.id;
    let mut agent_updates = agent.subscribe(Some((view.r#gen, view.seq)))?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the completed observer probe".into(),
            }],
        })
        .await?;
    let Reply::Accepted { turn, .. } = reply else {
        return Err(io::Error::other("probe prompt was not accepted").into());
    };
    wait_turn_end(&mut agent_updates, turn, Stop::EndTurn).await?;
    drop(agent_updates);
    Ok(ProbeSession { agent, session })
}

fn read_pid(path: &Path) -> Result<Option<u32>, TestError> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text.trim().parse()?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn journal_bytes(data_root: &Path, id: SessionId) -> Option<u64> {
    session_dir(data_root, id)
        .and_then(|dir| fs::metadata(dir.join("journal.jsonl")).ok())
        .map(|metadata| metadata.len())
}

fn session_dir(data_root: &Path, id: SessionId) -> Option<PathBuf> {
    let sessions = data_root.join("sessions");
    for workspace_dir in fs::read_dir(&sessions).ok()?.flatten() {
        let dir = workspace_dir.path().join(id.to_string());
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

/// Returns the tail of the detached exec job's captured log, when the job
/// produced output. The log names what the spawned chain actually did.
fn job_log_tail(data_root: &Path, id: SessionId, call: &str) -> String {
    let Some(log) =
        session_dir(data_root, id).map(|dir| dir.join("jobs").join(format!("{call}.log")))
    else {
        return "no session dir".to_string();
    };
    match fs::read(&log) {
        Ok(bytes) => {
            let tail = if bytes.len() > 2048 {
                &bytes[bytes.len() - 2048..]
            } else {
                &bytes[..]
            };
            format!("{tail:?}", tail = String::from_utf8_lossy(tail))
        }
        Err(error) => format!("unreadable: {error}"),
    }
}

fn job_state_digest(job: &ProcessJob, data_root: &Path) -> String {
    job.agent.view(PageReq::default()).map_or_else(
        |error| format!("view failed: {error}"),
        |view| {
            let mut tools = 0usize;
            let mut first_error = String::new();
            let mut first_result = String::new();
            for entry in &view.entries.items {
                if let EntryKind::ToolResult { error, parts, .. } = &entry.kind {
                    tools += 1;
                    if first_result.is_empty() {
                        first_result = format!("{parts:?}");
                    }
                    if *error && first_error.is_empty() {
                        first_error = format!("{parts:?}");
                    }
                }
            }
            let entries = view.entries.items.len();
            let journal = journal_bytes(data_root, view.session.id)
                .map_or_else(|| "missing".to_string(), |bytes| bytes.to_string());
            let mut text = format!(
                "turn={:?} entries={entries} tools={tools} journal={journal}",
                view.turn
            );
            if !first_error.is_empty() {
                text.push_str(" err=");
                text.push_str(&first_error);
            }
            if !first_result.is_empty() {
                text.push_str(" res=");
                text.push_str(&first_result);
            }
            if let Some(index) = job
                .pid_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| {
                    name.strip_prefix("child-")
                        .and_then(|name| name.strip_suffix(".pid"))
                })
            {
                text.push_str(" log=");
                text.push_str(&job_log_tail(
                    data_root,
                    view.session.id,
                    &format!("process-{index}"),
                ));
            }
            text
        },
    )
}

async fn wait_for_process_pids(
    jobs: &mut [ProcessJob],
    data_root: &Path,
) -> Result<Vec<u32>, TestError> {
    // Liveness wait, not a timing claim: 200 process spawns on a loaded shared
    // runner can far outrun the local constant, so bound generously. Hosted
    // macOS storage syncs orders of magnitude slower under create bursts, and
    // the arm64 Windows runner emulates the bash and PowerShell chain a
    // process at a time, so those bounds are wider for the same liveness
    // purpose.
    const PID_WAIT: Duration =
        if cfg!(target_os = "macos") || cfg!(all(windows, target_arch = "aarch64")) {
            Duration::from_secs(360)
        } else {
            Duration::from_secs(120)
        };
    let deadline = tokio::time::Instant::now() + PID_WAIT;
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
            let missing: Vec<String> = jobs
                .iter()
                .enumerate()
                .filter(|(_, job)| job.pid.is_none())
                .take(5)
                .map(|(index, job)| format!("{index}: {}", job_state_digest(job, data_root)))
                .collect();
            let total = jobs.iter().filter(|job| job.pid.is_none()).count();
            return Err(io::Error::other(format!(
                "not every process wrote its child pid ({total} missing of {}; first: {})",
                jobs.len(),
                missing.join(", ")
            ))
            .into());
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
    let binary = support::dalgon_binary()?;
    let token_output = ProcessCommand::new(binary)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "token"])
        .output()?;
    if !token_output.status.success() {
        return Err(io::Error::other("could not create WebSocket test token").into());
    }
    let token = String::from_utf8(token_output.stdout)?.trim().to_owned();
    let serve_log = dir.path().join("serve.log");
    let mut serve = ProcessCommand::new(binary);
    serve
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", &data_home)
        .args(["serve", "--bind", "127.0.0.1", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(File::create(&serve_log)?));
    // Winsock resolves its provider DLLs through SystemRoot; a cleared
    // environment leaves WSAStartup unable to initialize (os error 10106).
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        serve.env("SystemRoot", root);
    }
    let child = serve.spawn()?;
    let server = ServerProcess(Some(child));
    let url = advertised_websocket(&data_root)
        .await
        .map_err(|error| -> TestError {
            let log = fs::read_to_string(&serve_log).unwrap_or_default();
            format!("{error}\nserve stderr:\n{log}").into()
        })?;
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

/// Captures the variables the Windows shell ladder and spawned tools need;
// tokio keeps two kinds of kernel objects alive for the runtime's
// whole life: blocking-pool threads never exit (no idle timeout, 512
// cap), and its driver parks a small fixed set of events and ports once
// work saturates it. Handle growth beyond those is a product leak.
const RUNTIME_HANDLE_SLACK: usize = 64;

fn live_proc_count() -> usize {
    #[cfg(windows)]
    {
        dal_agent::live_procs()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Threads live in this process. Windows `HandleCount` covers thread
/// handles while unix `/dev/fd` never did, so the Windows check subtracts
/// thread growth instead of comparing raw totals.
#[cfg_attr(
    not(windows),
    expect(
        clippy::unnecessary_wraps,
        reason = "the Windows arm shells out to powershell and can fail"
    )
)]
fn open_thread_count() -> io::Result<usize> {
    #[cfg(windows)]
    {
        let command = format!(
            "(Get-Process -Id {} | Select-Object -ExpandProperty Threads).Count",
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
    #[cfg(not(windows))]
    {
        Ok(0)
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

/// The loaded-terminal proof: the real `dalgon` TUI on a pseudoterminal,
/// attached over a loopback WebSocket to the host that carries the load.
///
/// Each observation binds terminal output to the input that caused it. An
/// echo sample types a glyph that no other output can contain and waits for
/// that glyph in the bytes written after the key. A cancel sample runs in a
/// fresh terminal whose only turn is the one being cancelled, so the
/// cancelled notice cannot be left over from an earlier sample. Samplers run
/// on their own threads, because the host and its WebSocket listener need
/// the test runtime while the terminal is read.
#[cfg(unix)]
mod loaded_tui {
    use std::{
        collections::BTreeMap,
        io,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use dal_agent::{Agent, Host, SessionRef};
    use dal_core::{ApprovalMode, ClientId, Command, PageReq, Reply, Save, SessionId, Workspace};
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    use crate::{
        PROCESS_JOBS, TERMINAL_MODEL, TestError,
        pty::{self, PtyProcess, vt::VtRecorder},
        read_pid,
        support::TestDir,
        synthetic_route,
    };

    /// Loaded keypress echo bound on the timing lane: P99 below 24 ms.
    pub(super) const ECHO_P99: Duration = Duration::from_millis(24);
    /// Terminals driven on a shared runner, where counts are asserted but no
    /// latency is.
    pub(super) const SHARED_SAMPLES: usize = 3;
    /// Most echo samples one run takes; the sampler otherwise runs until the
    /// process storm has finished.
    const MAX_ECHO_SAMPLES: u32 = 1000;
    const COLUMNS: u16 = 100;
    const ROWS: u16 = 30;
    /// The status word while a tool call runs.
    const WORKING: &[&str] = &[dal_tui::copy::ids::STATE_WORKING];
    /// Every status word a turn shows while it runs; none is on screen once
    /// the turn has ended.
    const BUSY: &[&str] = &[
        dal_tui::copy::ids::STATE_WORKING,
        dal_tui::copy::ids::STATE_THINKING,
    ];
    // Liveness bounds, not timing claims: a terminal attached to a host
    // carrying 700 sessions and 200 process chains can start slowly on a
    // shared runner.
    const READY_WAIT: Duration = Duration::from_secs(120);
    const OUTPUT_WAIT: Duration = Duration::from_secs(60);
    const PID_WAIT: Duration = if cfg!(target_os = "macos") {
        Duration::from_secs(360)
    } else {
        Duration::from_secs(120)
    };
    const EXIT_WAIT: Duration = Duration::from_secs(30);
    /// How often a sampler drains the terminal while it waits for output.
    const POLL: Duration = Duration::from_micros(200);
    const STARTUP_QUIET: Duration = Duration::from_millis(500);
    const STARTUP_LIMIT: Duration = Duration::from_secs(10);
    const KEY_QUIET: Duration = Duration::from_millis(15);
    const KEY_LIMIT: Duration = Duration::from_millis(500);

    /// The in-process host served on loopback, plus the home the terminal
    /// clients run in.
    pub(super) struct LoadedTerminal {
        url: String,
        client_home: TestDir,
        stop: CancellationToken,
        serve: tokio::task::JoinHandle<Result<(), dal_wire::WireError>>,
    }

    /// A terminal typing glyphs into the loaded host's composer.
    pub(super) struct EchoSampler {
        pub(super) session: SessionId,
        stop: Arc<AtomicBool>,
        done: oneshot::Receiver<io::Result<Vec<Duration>>>,
        thread: JoinHandle<()>,
        _owner: Agent,
    }

    /// Latencies and recorded process ids of the cancelled terminal turns,
    /// all run in one session.
    pub(super) struct CancelRun {
        pub(super) session: SessionId,
        pub(super) latencies: Vec<Duration>,
        pub(super) pids: Vec<u32>,
    }

    struct CancelSample {
        latency: Duration,
        pid: u32,
    }

    /// The visible screen of a terminal, replayed from its raw output.
    struct ScreenWatch {
        vt: VtRecorder,
        fed: usize,
    }

    impl ScreenWatch {
        fn new() -> Self {
            Self {
                vt: VtRecorder::new(COLUMNS, ROWS),
                fed: 0,
            }
        }

        /// Replays the bytes written since the last call, then reports
        /// whether any visible row shows one of `words`.
        fn shows(&mut self, terminal: &PtyProcess, words: &[&str]) -> bool {
            let output = terminal.output();
            self.vt.feed(&output[self.fed..]);
            self.fed = output.len();
            self.vt
                .screen_rows()
                .iter()
                .any(|row| words.iter().any(|word| row.contains(word)))
        }
    }

    impl LoadedTerminal {
        /// Serves `host` on a loopback port for the terminal clients.
        pub(super) async fn serve(
            host: &Host,
            workspace: &Workspace,
            data_root: &Path,
        ) -> Result<Self, TestError> {
            let options = dal_wire::router::RouterOptions {
                bind: "127.0.0.1".to_owned(),
                port: 0,
                public: false,
                a2a: false,
                token_file: data_root.join("loaded-terminal.token"),
                approval: ApprovalMode::Ask,
                origins: Vec::new(),
                aliases: BTreeMap::new(),
                workspace: workspace.as_path().to_path_buf(),
            };
            let stop = CancellationToken::new();
            let handle = dal_wire::serve_router(host.clone(), options, stop.clone()).await?;
            let url = format!("ws://{}/v1/ws", handle.local_addr());
            let serve = tokio::spawn(handle.wait());
            Ok(Self {
                url,
                client_home: TestDir::new()?,
                stop,
                serve,
            })
        }

        /// Starts a terminal on a new session and returns once it is ready to
        /// be typed into, so sampling overlaps whatever load starts next.
        pub(super) async fn start_echo(
            &self,
            host: &Host,
            workspace: &Workspace,
            minimum: usize,
        ) -> Result<EchoSampler, TestError> {
            let (owner, session) = open_session(host, workspace, None).await?;
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let (ready_tx, ready_rx) = oneshot::channel();
            let (done_tx, done_rx) = oneshot::channel();
            let url = self.url.clone();
            let home = self.client_home.path().to_path_buf();
            let thread = thread::Builder::new()
                .name("stress-echo-sampler".to_owned())
                .spawn(move || {
                    let result = sample_echo(&url, session, &home, minimum, &flag, ready_tx);
                    let _ = done_tx.send(result);
                })?;
            if ready_rx.await.is_err() {
                // The sampler ended before it typed: its own error says why.
                let result = done_rx.await;
                join(thread)?;
                return Err(match result {
                    Ok(Err(error)) => error.into(),
                    Ok(Ok(_)) => {
                        io::Error::other("the echo sampler stopped before its terminal was ready")
                            .into()
                    }
                    Err(error) => error.into(),
                });
            }
            Ok(EchoSampler {
                session,
                stop,
                done: done_rx,
                thread,
                _owner: owner,
            })
        }

        /// Runs `samples` exec turns in one terminal session and cancels each
        /// with Ctrl+C, recording how long the terminal took to stop showing
        /// the turn as busy after the key.
        pub(super) async fn sample_cancels(
            &self,
            host: &Host,
            workspace: &Workspace,
            samples: usize,
        ) -> Result<CancelRun, TestError> {
            let (_owner, session) = open_session(host, workspace, Some(TERMINAL_MODEL)).await?;
            let url = self.url.clone();
            let home = self.client_home.path().to_path_buf();
            let jobs = workspace.as_path().to_path_buf();
            let (done_tx, done_rx) = oneshot::channel();
            let thread = thread::Builder::new()
                .name("stress-cancel-sampler".to_owned())
                .spawn(move || {
                    let _ = done_tx.send(cancel_all(&url, session, &home, &jobs, samples));
                })?;
            let result = done_rx.await;
            join(thread)?;
            let taken = result??;
            Ok(CancelRun {
                session,
                latencies: taken.iter().map(|sample| sample.latency).collect(),
                pids: taken.iter().map(|sample| sample.pid).collect(),
            })
        }

        /// Stops the listener once every terminal has exited.
        pub(super) async fn stop(self) -> Result<(), TestError> {
            self.stop.cancel();
            self.serve.await??;
            Ok(())
        }
    }

    impl EchoSampler {
        /// Ends sampling after the minimum count and returns every echo latency.
        pub(super) async fn finish(self) -> Result<Vec<Duration>, TestError> {
            self.stop.store(true, Ordering::Release);
            let result = self.done.await;
            join(self.thread)?;
            Ok(result??)
        }
    }

    fn join(thread: JoinHandle<()>) -> Result<(), TestError> {
        thread
            .join()
            .map_err(|_| io::Error::other("a loaded-terminal sampler thread panicked").into())
    }

    /// Opens a session on the host. The terminal attaches to it by id, so a
    /// model chosen here is the model the terminal's turns run on.
    async fn open_session(
        host: &Host,
        workspace: &Workspace,
        model: Option<&str>,
    ) -> Result<(Agent, SessionId), TestError> {
        let agent = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("stress-terminal"),
            )
            .await?;
        if let Some(model) = model {
            let reply = agent
                .submit(Command::SetModel {
                    model: synthetic_route(model),
                    save: Save::SessionOnly,
                })
                .await?;
            assert!(matches!(reply, Reply::Done(_)));
        }
        let id = agent.view(PageReq::default())?.session.id;
        Ok((agent, id))
    }

    fn spawn_remote_tui(home: &Path, url: &str, session: SessionId) -> io::Result<PtyProcess> {
        let mut command = pty::dalgon_command(home, &["the host supplies the provider"])?;
        command.args([
            "--screen",
            "inline",
            "--connect",
            url,
            &format!("--resume={session}"),
        ]);
        let mut terminal = PtyProcess::spawn(&mut command, COLUMNS, ROWS)?;
        terminal.wait_for(
            dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
            READY_WAIT,
        )?;
        Ok(terminal)
    }

    fn sample_echo(
        url: &str,
        session: SessionId,
        home: &Path,
        minimum: usize,
        stop: &AtomicBool,
        ready: oneshot::Sender<()>,
    ) -> io::Result<Vec<Duration>> {
        let mut terminal = spawn_remote_tui(home, url, session)?;
        // The first frame paints before the terminal probes finish; keys wait
        // for startup to go quiet so launch work is not counted as an echo.
        settle(&mut terminal, STARTUP_QUIET, STARTUP_LIMIT)?;
        // The TUI owns echo only when it has turned the line discipline's
        // echo off. With it on, the glyph below would be the kernel's reply.
        if terminal
            .termios()?
            .local_modes
            .contains(rustix::termios::LocalModes::ECHO)
        {
            return Err(io::Error::other(
                "the terminal kept kernel echo on, so the measured echo would not be the TUI's",
            ));
        }
        let _ = ready.send(());
        let mut samples = Vec::with_capacity(minimum);
        for index in 0..MAX_ECHO_SAMPLES {
            if samples.len() >= minimum && stop.load(Ordering::Acquire) {
                break;
            }
            samples.push(echo_once(&mut terminal, index)?);
        }
        quit(&mut terminal)?;
        Ok(samples)
    }

    /// Types one glyph and returns the time until it is written back. Each
    /// sample types a different CJK ideograph, which no frame, hint, or
    /// status text prints, so only the echo of this key can contain it.
    fn echo_once(terminal: &mut PtyProcess, index: u32) -> io::Result<Duration> {
        let glyph = char::from_u32(0x4E00 + index)
            .ok_or_else(|| io::Error::other("echo glyph is not a character"))?;
        let needle = glyph.to_string().into_bytes();
        let from = terminal.output().len();
        let started = Instant::now();
        terminal.write(&needle)?;
        let seen = wait_for_output(terminal, from, &needle, OUTPUT_WAIT)?;
        // Erase the glyph so the composer stays one cell wide, and let the
        // repaint finish before the next key.
        terminal.write(b"\x7f")?;
        settle(terminal, KEY_QUIET, KEY_LIMIT)?;
        Ok(seen.duration_since(started))
    }

    fn cancel_all(
        url: &str,
        session: SessionId,
        home: &Path,
        jobs: &Path,
        samples: usize,
    ) -> io::Result<Vec<CancelSample>> {
        let mut terminal = spawn_remote_tui(home, url, session)?;
        let mut watch = ScreenWatch::new();
        settle(&mut terminal, STARTUP_QUIET, STARTUP_LIMIT)?;
        let mut taken = Vec::with_capacity(samples);
        for offset in 0..samples {
            // Job numbers past the API-driven jobs keep each pid file distinct.
            let job = PROCESS_JOBS + offset;
            let pid_path = jobs.join(format!("child-{job}.pid"));
            taken.push(cancel_once(&mut terminal, &mut watch, job, &pid_path)?);
        }
        quit(&mut terminal)?;
        Ok(taken)
    }

    /// Runs one exec turn and cancels it with Ctrl+C.
    ///
    /// The cancelled notice cannot bind a sample: the TUI keeps the last
    /// three notices on screen and never clears them, and every cancel adds
    /// the same text. The busy word in the status row does bind it. The
    /// terminal is idle before the prompt, so a busy word that appears is
    /// this turn's, and the exec chain is running when the key is written,
    /// so the word can only leave because of that key.
    fn cancel_once(
        terminal: &mut PtyProcess,
        watch: &mut ScreenWatch,
        job: usize,
        pid_path: &Path,
    ) -> io::Result<CancelSample> {
        settle(terminal, KEY_QUIET, KEY_LIMIT)?;
        if watch.shows(terminal, BUSY) {
            return Err(io::Error::other(
                "the terminal still showed a busy turn before the next prompt",
            ));
        }
        terminal.write(format!("process-job-{job}\r").as_bytes())?;
        let pid = wait_for_pid(terminal, pid_path)?;
        wait_for_screen(terminal, watch, WORKING, |shown| shown)?;
        settle(terminal, KEY_QUIET, KEY_LIMIT)?;
        let started = Instant::now();
        terminal.write(b"\x03")?;
        let seen = wait_for_screen(terminal, watch, BUSY, |shown| !shown)?;
        Ok(CancelSample {
            latency: seen.duration_since(started),
            pid,
        })
    }

    /// Drains the terminal until `wanted` accepts whether the visible screen
    /// shows one of `words`, and returns when that was seen.
    fn wait_for_screen(
        terminal: &mut PtyProcess,
        watch: &mut ScreenWatch,
        words: &[&str],
        wanted: impl Fn(bool) -> bool,
    ) -> io::Result<Instant> {
        let deadline = Instant::now() + OUTPUT_WAIT;
        loop {
            terminal.collect_for(Duration::ZERO)?;
            if wanted(watch.shows(terminal, words)) {
                return Ok(Instant::now());
            }
            if Instant::now() >= deadline {
                return Err(unseen(
                    terminal,
                    &format!("the wanted screen state for {words:?}"),
                ));
            }
            thread::sleep(POLL);
        }
    }

    fn wait_for_pid(terminal: &mut PtyProcess, path: &Path) -> io::Result<u32> {
        let deadline = Instant::now() + PID_WAIT;
        loop {
            terminal.collect_for(Duration::from_millis(5))?;
            if let Some(pid) = read_pid(path).map_err(io::Error::other)? {
                return Ok(pid);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "{} never recorded a child pid",
                    path.display()
                )));
            }
        }
    }

    /// Drains the terminal until `needle` appears in bytes written after
    /// offset `from`, and returns when it was seen.
    fn wait_for_output(
        terminal: &mut PtyProcess,
        from: usize,
        needle: &[u8],
        timeout: Duration,
    ) -> io::Result<Instant> {
        let deadline = Instant::now() + timeout;
        loop {
            terminal.collect_for(Duration::ZERO)?;
            if pty::occurrences(&terminal.output()[from..], needle) > 0 {
                return Ok(Instant::now());
            }
            if Instant::now() >= deadline {
                return Err(unseen(
                    terminal,
                    &format!("{:?}", String::from_utf8_lossy(needle)),
                ));
            }
            thread::sleep(POLL);
        }
    }

    fn unseen(terminal: &PtyProcess, what: &str) -> io::Error {
        let captured = String::from_utf8_lossy(terminal.output());
        let tail = captured
            .get(captured.len().saturating_sub(2048)..)
            .unwrap_or(&captured);
        io::Error::other(format!(
            "terminal output never reached {what}\ncaptured tail:\n{tail}"
        ))
    }

    /// Drains the terminal until it has written nothing for `quiet`, or
    /// `limit` has passed.
    fn settle(terminal: &mut PtyProcess, quiet: Duration, limit: Duration) -> io::Result<()> {
        let started = Instant::now();
        let mut seen = terminal.output().len();
        let mut changed = started;
        loop {
            terminal.collect_for(Duration::ZERO)?;
            let now = Instant::now();
            let length = terminal.output().len();
            if length != seen {
                seen = length;
                changed = now;
            }
            if now.duration_since(changed) >= quiet || now.duration_since(started) >= limit {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Quits on the empty composer with Ctrl+D and requires a clean exit.
    fn quit(terminal: &mut PtyProcess) -> io::Result<()> {
        settle(terminal, KEY_QUIET, KEY_LIMIT)?;
        terminal.write(b"\x04")?;
        let status = terminal.wait_for_exit(EXIT_WAIT)?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "the remote TUI exited with {status}"
            )))
        }
    }
}

#[expect(clippy::too_many_lines, reason = "SC gate is one long stress scenario")]
async fn full_load_scenario() -> Result<(), TestError> {
    // Wall-clock and resource bounds assert only on the nightly
    // idle-machine lane; shared CI runners cannot hold them.
    let budgets = std::env::var_os("DAL_TIMING_BUDGETS").is_some();
    // The loaded keypress echo and cancel bounds are measured on the real
    // TUI through a pseudoterminal. No pseudoterminal harness exists for
    // this target, so the timing lane cannot be satisfied here: fail instead
    // of passing without the proof.
    #[cfg(not(unix))]
    if budgets {
        return Err(io::Error::other(
            "the loaded keypress-echo and cancel proof needs the real TUI on a \
             pseudoterminal, and this gate has no terminal harness for this target",
        )
        .into());
    }
    #[cfg(unix)]
    let tui_samples = if budgets {
        CANCELLATIONS
    } else {
        loaded_tui::SHARED_SAMPLES
    };
    #[cfg_attr(
        not(unix),
        expect(unused_mut, reason = "only unix targets attach terminals")
    )]
    let mut tui_sessions = TuiSessions::default();
    let data = TestDir::new()?;
    let workspace_dir = TestDir::new()?;
    let workspace = Workspace::new(workspace_dir.path().to_path_buf())?;
    let fixture_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/replay/stress-scripted.jsonl");
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
    let pre_run_threads = open_thread_count()?;
    let env = Env {
        vars: support::captured_shell_vars(),
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
    if budgets {
        assert!(idle_p99 < CANCEL_P99, "idle cancel p99 was {idle_p99:?}");
    }
    // The completed-call probe runs before the storm, when no load can
    // wedge its live turn: one fast exec call settles while the turn stays
    // open, so the result observer must fire exactly once by contract.
    let probe = start_probe_session(&host, &workspace).await?;
    // Bind and attach the WebSocket clients before the process storm;
    // the assertions exercise live clients under load, and Windows
    // WSAStartup transiently fails when a spawn lands at peak procs.
    let web_dir = TestDir::new()?;
    let (server, websocket_url, token) = spawn_websocket_server(&web_dir).await?;
    let web_sockets = connect_websocket_clients(&websocket_url, &token).await?;
    // The real TUI attaches to this loaded host over a loopback WebSocket
    // before the process storm and types through it while the storm runs.
    #[cfg(unix)]
    let terminal = loaded_tui::LoadedTerminal::serve(&host, &workspace, data.path()).await?;
    #[cfg(unix)]
    let echo = terminal.start_echo(&host, &workspace, tui_samples).await?;
    let mut process_jobs = start_process_jobs(&host, &workspace).await?;
    let pids = wait_for_process_pids(&mut process_jobs, data.path()).await?;
    #[cfg(unix)]
    {
        tui_sessions.echo = Some(echo.session);
        let echo_samples = echo.finish().await?;
        let echo_p99 = nearest_rank_p99(&echo_samples);
        if budgets {
            assert!(
                echo_p99 < loaded_tui::ECHO_P99,
                "loaded keypress echo p99 was {echo_p99:?} over {} samples",
                echo_samples.len()
            );
        }
    }
    let after_jobs_spawn = open_handle_count()?;
    let rss = resident_set_bytes()?;
    let handles = open_handle_count()?;
    if budgets {
        assert!(
            rss < RESOURCE_LIMIT_BYTES,
            "resident memory was {rss} bytes"
        );
        assert!(handles < HANDLE_LIMIT, "open handle count was {handles}");
    }
    // Every job session and child session is still open with its process
    // chain running: the terminal cancels below happen at peak load.
    #[cfg(unix)]
    {
        let cancels = terminal
            .sample_cancels(&host, &workspace, tui_samples)
            .await?;
        // The 201 job chains ran through every terminal turn above, so the
        // latencies were taken with the whole process load alive.
        let alive = live_processes(&pids)?.len();
        assert_eq!(
            alive, PROCESS_JOBS,
            "only {alive} of {PROCESS_JOBS} job process chains survived the terminal samples"
        );
        wait_for_processes_to_exit(&cancels.pids).await?;
        terminal.stop().await?;
        let tui_cancel_p99 = nearest_rank_p99(&cancels.latencies);
        if budgets {
            assert!(
                tui_cancel_p99 < CANCEL_P99,
                "loaded terminal cancel p99 was {tui_cancel_p99:?} over {} samples",
                cancels.latencies.len()
            );
        }
        tui_sessions.cancel = Some((cancels.session, cancels.latencies.len()));
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
    let after_first_cancel = open_handle_count()?;
    let remaining_samples = cancel_jobs(
        &mut process_jobs[CANCELLATIONS..],
        PROCESS_JOBS - CANCELLATIONS,
    )
    .await?;
    assert_eq!(remaining_samples.len(), PROCESS_JOBS - CANCELLATIONS);
    wait_for_processes_to_exit(&pids).await?;
    let after_jobs_exit = open_handle_count()?;
    let expected_root_turns = idle_samples.len() + 1;
    drop(web_sockets);
    drop(server);
    let report = host.shutdown(Duration::from_secs(30)).await;
    let after_shutdown = open_handle_count()?;
    // The root's session-end sweep cascade-closes the children before the
    // shutdown loop reaches them, so `sessions_closed` only ever counts the
    // top-level sessions plus the children the loop got to first — every
    // session ending exactly once is asserted by `session_ends` below.
    assert!(report.sessions_closed > PROCESS_JOBS);
    assert_eq!(report.tasks_remaining, 0);
    // The probe session opened one extra top-level session on the same
    // hook counters, so lifecycle totals count it alongside the jobs.
    assert_eq!(
        hooks.session_starts.load(Ordering::Acquire),
        CHILD_SESSIONS + PROCESS_JOBS + 2 + tui_sessions.count()
    );
    assert_eq!(
        hooks.session_ends.load(Ordering::Acquire),
        CHILD_SESSIONS + PROCESS_JOBS + 2 + tui_sessions.count()
    );
    assert!(hooks.inputs.load(Ordering::Acquire) > 0);
    assert!(hooks.before_turns.load(Ordering::Acquire) > 0);
    assert!(hooks.before_requests.load(Ordering::Acquire) > 0);
    let tool_calls = hooks.tool_calls.load(Ordering::Acquire);
    assert!(tool_calls >= CANCELLATIONS);
    // Per-call result proof keyed by admitted (session, call) pairs.
    assert_tool_result_delivery(&hooks, &probe)?;
    assert_cancelled_job_journals(&process_jobs[..CANCELLATIONS], &hooks)?;
    assert!(hooks.turn_ends.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.settled.load(Ordering::Acquire) >= CANCELLATIONS);
    assert!(hooks.stream_starts.load(Ordering::Acquire) > 0);
    assert!(hooks.stream_deltas.load(Ordering::Acquire) > 0);
    assert!(hooks.stream_finishes.load(Ordering::Acquire) > 0);
    let mut session_ids = expected_session_ids(root_id, &process_jobs, &reports, &hooks);
    assert_eq!(session_ids.len(), CHILD_SESSIONS + PROCESS_JOBS + 1);
    session_ids.push(probe.session);
    session_ids.extend(tui_sessions.echo);
    session_ids.extend(tui_sessions.cancel.map(|(id, _)| id));
    {
        let ends_by_session = locked(&hooks.ends_by_session);
        assert_eq!(ends_by_session.len(), session_ids.len());
        assert!(
            session_ids
                .iter()
                .all(|id| ends_by_session.get(id) == Some(&1))
        );
        assert!(
            ends_by_session.get(&probe.session) == Some(&1),
            "probe session ended exactly once"
        );
    }
    verify_journal_ends(
        data.path(),
        &workspace,
        &session_ids,
        root_id,
        expected_root_turns,
        &tui_sessions,
        Some(probe.session),
    )
    .await?;
    drop(process_jobs);
    drop(idle_updates);
    drop(root);
    // tokio keeps two kinds of kernel objects alive for the runtime's
    // whole life: blocking-pool threads never exit (no idle timeout, 512
    // cap), and its driver parks a small fixed set of events/ports once
    // work saturates it. Only growth beyond those is a product leak.
    let mut after_handles = open_handle_count()?;
    let mut after_threads = open_thread_count()?;
    let settled = |handles: usize, threads: usize| {
        handles.saturating_sub(pre_run_handles)
            <= threads.saturating_sub(pre_run_threads) + RUNTIME_HANDLE_SLACK
    };
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !settled(after_handles, after_threads) && tokio::time::Instant::now() < settle_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        after_handles = open_handle_count()?;
        after_threads = open_thread_count()?;
    }
    let live = live_proc_count();
    let growth = after_handles.saturating_sub(pre_run_handles);
    let thread_growth = after_threads.saturating_sub(pre_run_threads);
    assert!(
        growth <= thread_growth + RUNTIME_HANDLE_SLACK,
        "non-thread kernel handles did not return to baseline \
         (handles={after_handles} baseline={pre_run_handles} \
         threads={after_threads} baseline-threads={pre_run_threads} \
         spawn={after_jobs_spawn} first-cancel={after_first_cancel} \
         jobs-exit={after_jobs_exit} shutdown={after_shutdown} live-procs={live})"
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
            dal_core::Block::Text { text, .. } => Some(text.as_ref()),
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
    tui: &TuiSessions,
    probe_session: Option<SessionId>,
) -> Result<(), TestError> {
    let store = Store::new(
        data_root.to_path_buf(),
        workspace.clone(),
        StoreProduct::Dal,
    );
    for id in session_ids {
        if tui.echo == Some(*id) {
            // The echo-only session never held a turn, and a journal is
            // written with its first entry, so it has none to open.
            continue;
        }
        let (mut journal, _) = store.open_session(*id).await?;
        // One entry per turn-end record: whether that turn ended cancelled.
        let cancelled: Vec<bool> = journal
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::TurnEnd { stop, .. } => Some(*stop == dal_core::TurnEndStop::Cancelled),
                _ => None,
            })
            .collect();
        let terminal_turns = tui
            .cancel
            .filter(|(cancelled_id, _)| cancelled_id == id)
            .map(|(_, turns)| turns);
        let expected = if *id == root_id {
            root_turns
        } else if probe_session == Some(*id) {
            1
        } else {
            terminal_turns.unwrap_or(1)
        };
        assert_eq!(
            cancelled.len(),
            expected,
            "session {id} had {} turn-end records",
            cancelled.len()
        );
        if probe_session == Some(*id) {
            assert!(
                cancelled.iter().all(|ended| !*ended),
                "probe session {id} ended its single turn as cancelled"
            );
        }
        if terminal_turns.is_some() {
            assert!(
                cancelled.iter().all(|ended| *ended),
                "terminal session {id} did not end every turn as cancelled"
            );
        }
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
            vars: support::captured_shell_vars(),
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
        (PhaseModel::Opening(expected), Phase::Opening { turn, .. })
        | (PhaseModel::Running(expected), Phase::Running { turn, .. }) => expected == turn,
        (PhaseModel::Idle, Phase::Idle) => true,
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
use std::collections::HashSet;

#[test]
fn stress_shuttle_schedules_preserve_actor_invariants() {
    let _serial = TEST_LOCK.blocking_lock();
    let data = TestDir::new().expect("actor smoke data directory");
    let data_root = data.path().to_path_buf();
    fs::create_dir_all(data_root.join("workspace")).expect("actor smoke workspace directory");
    actor_schedule(&data_root);

    // Shuttle controls every task poll in this half. No Tokio runtime, file
    // shard, process, or network handle crosses the Shuttle continuation.
    #[cfg(not(all(windows, target_arch = "aarch64")))]
    {
        const DEFAULT_SHUTTLE_SEED: u64 = 0x5eed_01a7_0c70_5e5d;
        // Keep one real actor smoke run beside the pure scheduler run. The
        // scheduler run is deliberately limited to Shuttle-aware futures and
        // the in-memory journal.
        let seed = std::env::var("SHUTTLE_RANDOM_SEED")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_SHUTTLE_SEED);
        shuttle::check_random_with_seed(shuttle_actor_schedule, seed, 32);
    }
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
type StoreJournal = dal_store::Journal;
#[cfg(not(all(windows, target_arch = "aarch64")))]
type ShuttleActor = Arc<ShuttleMutex<ShuttleActorState>>;

#[cfg(not(all(windows, target_arch = "aarch64")))]
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
fn shuttle_actor_state() -> ShuttleActorState {
    let workspace = Workspace::new(if cfg!(windows) {
        PathBuf::from("C:/shuttle-workspace")
    } else {
        PathBuf::from("/shuttle-workspace")
    })
    .expect("Shuttle workspace");
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
async fn shuttle_mail_task(actor: ShuttleActor, text: &'static str) {
    shuttle_future::yield_now().await;
    shuttle_record_mail(&actor, text);
}

#[cfg(not(all(windows, target_arch = "aarch64")))]
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
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

#[cfg(not(all(windows, target_arch = "aarch64")))]
async fn shuttle_tracked_task(actor: ShuttleActor) {
    shuttle_future::yield_now().await;
    let mut actor = actor.lock().expect("Shuttle actor state lock");
    actor.tracked_tasks = actor.tracked_tasks.saturating_sub(1);
}
