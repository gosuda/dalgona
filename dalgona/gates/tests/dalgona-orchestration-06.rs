// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the review battery refuses a round while
//! orchestration work is live in the session, and runs once the work ends.
//! One `agents` run starts, a `review` call in the same turn is refused with
//! the live-work text, and a second `review` after the run reports and its
//! wake delivers proceeds past the refusal.
//!
//! The model boundary is deterministic without a shared replay fixture: a
//! registered synthetic model (the public `ModelHandler` extension API, as
//! `dalgona-orchestration-09` uses it) answers each request from its own
//! context. A child whose tool surface carries `orchestration__report`
//! waits for the parent refusal and then reports `done`; the parent starts
//! the run, calls `review` once, ends the prompt turn, and after the
//! delivery wake calls `review` again into the git check. The shared FIFO
//! replay fixture cannot give that, because one queue serves every session
//! in script order.
#![expect(
    clippy::disallowed_methods,
    reason = "gate builds real git fixtures for the review tool"
)]
use dalgona_gates::support;

use std::fmt;
use std::fs;
use std::future::pending;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord, ScopeError, ScopeHandle, ScopeStatus,
};
use dal_agent::{Delivery, Subscription};
use dal_core::{
    Budget, Caps, ContextItem, InferFailure, ModelId, ModelRequest, ModelRoute, ModelToolSpec,
    OnError, Part, Purpose, RawJson, Reply, RequestParams, ScopeSpec, ServiceSet, Stop,
    ThinkingLevel, UpdateKind, Usage,
};
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs, ToolCall};

const RUN_CALL: &str = r#"{"action":"run","steps":[{"name":"solo","prompt":"do the thing"}]}"#;

const REPORT_CALL: &str = r#"{"status":"done","report":"did the thing"}"#;

const REVIEW_CALL: &str = r"{}";

const REPORT_BODY: &str = "did the thing";

const GATE_MODEL: &str = "review-live/gate";

const CHILD_MODEL: &str = "review-live/child";

const REPORT_TOOL_NAME: &str = "orchestration__report";

const WAIT: Duration = Duration::from_secs(30);

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The one synthetic model the gate registers: every request is answered
/// from the request's own tool surface and context, so no shared step queue
/// exists. The child waits for the parent refusal before it reports.
struct ReviewLiveModel {
    refused: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
}

impl ModelHandler for ReviewLiveModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            if child_reports(request.tools.as_ref()) {
                if context_saw_tool(request.context.as_ref(), REPORT_TOOL_NAME) {
                    return Ok(text_stream("ack"));
                }
                if !self.refused.load(Ordering::Acquire) {
                    let notified = self.wake.notified();
                    if !self.refused.load(Ordering::Acquire) {
                        notified.await;
                    }
                }
                return tool_call_stream("c1", REPORT_TOOL_NAME, REPORT_CALL)
                    .map_err(|_| ModelError::SecondForward);
            }
            let saw_agents = context_saw_tool(request.context.as_ref(), "agents");
            let reviews = review_count(request.context.as_ref());
            if !saw_agents {
                return tool_call_stream("r1", "agents", RUN_CALL)
                    .map_err(|_| ModelError::SecondForward);
            }
            if reviews == 0 {
                return tool_call_stream("v1", "review", REVIEW_CALL)
                    .map_err(|_| ModelError::SecondForward);
            }
            if reviews == 1 {
                if context_has_delivery(request.context.as_ref()) {
                    return tool_call_stream("v2", "review", REVIEW_CALL)
                        .map_err(|_| ModelError::SecondForward);
                }
                self.refused.store(true, Ordering::Release);
                self.wake.notify_waiters();
                return Ok(text_stream("started"));
            }
            Ok(text_stream("closing"))
        })
    }
}

/// Whether the context carries a tool result for `name`.
fn context_saw_tool(context: &[ContextItem], name: &str) -> bool {
    context.iter().any(|item| {
        matches!(
            item,
            ContextItem::ToolResult { name: seen, .. } if seen.as_ref() == name
        )
    })
}

/// How many `review` tool results the context carries.
fn review_count(context: &[ContextItem]) -> usize {
    context
        .iter()
        .filter(|item| {
            matches!(
                item,
                ContextItem::ToolResult { name: seen, .. } if seen.as_ref() == "review"
            )
        })
        .count()
}

/// Whether the context carries the run delivery: a user message holding the
/// child report body. The prompt turn has no such message; the delivery wake
/// does.
fn context_has_delivery(context: &[ContextItem]) -> bool {
    context.iter().any(|item| match item {
        ContextItem::User { parts } => parts.iter().any(|part| match part {
            Part::Text { text } => text.contains(REPORT_BODY),
            _ => false,
        }),
        _ => false,
    })
}

/// Whether the visible tool surface contains the child report tool: only such
/// a request can report.
fn child_reports(tools: &[ModelToolSpec]) -> bool {
    tools
        .iter()
        .any(|tool| tool.name.as_ref() == REPORT_TOOL_NAME)
}

/// Wraps one response's events into an `EventStream`.
fn stream_of(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
}

/// A failed model response that carries `cause` as its fatal text. `ModelError`
/// has no variant that carries a cause, and a variant that names another
/// failure (`SecondForward`, `PrivateRounds`) would replace the real cause
/// with an unrelated one. The cause therefore reaches the session the way the
/// engine reports its own failed model calls: one `InferFailure::Fatal`
/// stream item that displays exactly `cause`.
fn failed_stream(cause: &dyn fmt::Display) -> EventStream {
    let failure = InferFailure::Fatal {
        message: cause.to_string().into(),
        fix: None,
    };
    stream_of(vec![Err(ProviderError::Synthetic(failure))])
}

/// One text-only scripted-model response that ends the turn.
fn text_stream(text: &str) -> EventStream {
    stream_of(vec![
        Ok(StreamEvent::TextDelta {
            text: text.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ])
}

/// One tool-call response: the model calls `name` with `args` and stops for
/// the tool result.
///
/// # Errors
/// Returns the parse error when `args` is not one JSON value.
fn tool_call_stream(
    id: &str,
    name: &str,
    args: &str,
) -> Result<EventStream, dal_core::RawJsonError> {
    let raw = RawJson::parse(args)?;
    let call = ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        args: ToolArgs::Parsed(raw),
    };
    Ok(stream_of(vec![
        Ok(StreamEvent::ToolCallStarted {
            id: id.to_owned(),
            name: name.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: vec![call] }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ]))
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

/// The synthetic-model extension the gate adds to the assembled product.
fn live_extension() -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let refused = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(tokio::sync::Notify::new());
    let gate = live_record(GATE_MODEL, Arc::clone(&refused), Arc::clone(&wake))?;
    let child = live_record(CHILD_MODEL, refused, wake)?;
    ExtensionBuilder::new("review-live", "0.1.0", ServiceSet::default())?
        .model(gate)
        .model(child)
        .build()
}

fn live_record(
    id: &str,
    refused: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
) -> Result<ModelRecord, dal_core::RegistrationError> {
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: Caps {
            context_window: Some(200_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(ReviewLiveModel { refused, wake }),
        export: None,
    })
}

#[test]
fn review_refuses_a_round_while_a_run_is_live() -> support::TestResult<()> {
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-review-live-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    // Real git lifecycle: the workspace is its own repository so review's
    // `git diff`/`git status` never discover the outer checkout. One seed
    // commit leaves the tree clean, so the second review takes the
    // no-changes path after proving it is past the live-work refusal.
    init_git_repo(&workspace)?;
    // Parent turn: start the run, call review (refused), end the turn. The
    // child then reports and acks. The delivery wake turn calls review a
    // second time and proceeds past the refusal into the git check.
    let user = format!("model = \"{GATE_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let product = support::build_product(data.clone(), Some(&user))?;
        for extension in &product.extensions {
            let services = extension.inject().capabilities();
            if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
                continue;
            }
            dal_agent::ext::grants::GrantStore::new(data.clone())
                .grant(
                    dal_agent::ext::grants::GrantKey::for_extension(extension, &workspace)
                        .ok_or("registered extension has a code identity")?,
                    dal_core::ClientId::new("review-live-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let mut product = support::build_product(data.clone(), Some(&user))?;
        product.extensions.push(live_extension()?);
        let factory = dalgona::product();
        let config = dal_core::Config::load(
            dal_core::ConfigProduct::Dalgona,
            &data,
            factory.defaults,
            Some(&user),
        )?;
        let env = dal_agent::Env {
            vars: std::collections::BTreeMap::default(),
            cwd: data.clone(),
            sandbox_helper: None,
        };
        let host = dal_agent::Host::start(product, config, env).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace)?,
                    name: None,
                },
                dal_core::ClientId::new("review-live-gate"),
            )
            .await?;
        let session = agent
            .view(dal_core::PageReq::default())?
            .session
            .id
            .to_string();
        agent
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![dal_core::Part::Text {
                    text: "start the run".into(),
                }],
            })
            .await?;
        wait_for_journal(
            &data,
            &session,
            "orchestration work is still live in this session (1 job)",
            |journal| journal.contains("orchestration work is still live in this session (1 job)"),
        )
        .await?;
        wait_for_journal(
            &data,
            &session,
            "the delivery wake to end its turn",
            |journal| support::journal_records(journal, "turn_end").len() >= 2,
        )
        .await?;

        wait_for_journal(&data, &session, "No changes to review.", |journal| {
            journal.contains("No changes to review.")
        })
        .await?;

        let journal = read_journal(&data, &session).unwrap_or_default();
        let refusals = journal.matches("orchestration work is still live").count();
        assert_eq!(
            refusals, 1,
            "exactly the first review is refused for live work: {journal}"
        );
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

/// Inits `workspace` as its own git repository with one seed commit, so
/// review's `git diff HEAD` and `git status` run against this tree instead
/// of discovering the outer checkout. The clean tree makes the second
/// review return the deterministic no-changes text.
fn init_git_repo(workspace: &Path) -> support::TestResult<()> {
    let run = |args: &[&str]| -> support::TestResult<()> {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .map_err(|error| format!("git {} failed to run: {error}", args.join(" ")))?;
        if !status.success() {
            return Err(format!("git {} failed", args.join(" ")).into());
        }
        Ok(())
    };
    run(&["init"])?;
    run(&["config", "user.email", "test@example.com"])?;
    run(&["config", "user.name", "test"])?;
    fs::write(workspace.join("seed.txt"), b"seed")?;
    run(&["add", "-A"])?;
    run(&["-c", "commit.gpgsign=false", "commit", "-m", "seed"])?;
    Ok(())
}

/// Waits until `ready` holds for the parent session's journal. The error
/// names what was awaited and keeps the last journal read.
async fn wait_for_journal(
    data: &Path,
    session: &str,
    awaited: &str,
    ready: impl Fn(&str) -> bool,
) -> support::TestResult<()> {
    let mut last = String::new();
    let waited = tokio::time::timeout(WAIT, async {
        loop {
            last = read_journal(data, session).unwrap_or_default();
            if ready(&last) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    waited.map_err(|_| format!("the journal never showed {awaited:?}:\n{last}"))?;
    Ok(())
}

/// Reads the journal of the parent session, found by its session id. A child
/// session keeps its own journal beside it.
fn read_journal(data: &Path, session: &str) -> Option<String> {
    let mut journals = Vec::new();
    collect(&data.join("sessions"), &mut journals).ok()?;
    let path = journals.into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == session)
    })?;
    fs::read_to_string(path).ok()
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

// The hook-deadline gate: a real `before_turn` hook opens a scope and holds
// it past the hook deadline. The deadline is the runtime's own 5s hook
// budget (`HOOK_DEADLINE_MS`, engine/dal-agent/src/ext/hooks.rs, applied by
// the hook dispatch); this gate writes no timeout setting. The documented
// `hooks.deadline` TOML key is read by no code, so no config knob is set or
// claimed here.

/// The hanging member route the scope handles infer against.
const HOOK_MEMBER_ROUTE: &str = "hook-gate/member";

/// The Rust scope-owning child model the hook drives.
const HOOK_OWNER_ROUTE: &str = "hook-gate/owner";

/// The Starlark scope-owning child model the hook drives.
const STAR_HOLD_ROUTE: &str = "hook-deadline-scope/hold";

/// The Starlark plugin name behind `STAR_HOLD_ROUTE`.
const STAR_PLUGIN: &str = "hook-deadline-scope";

/// Held scope members per hook invocation; the criterion floor is three.
const HOOK_MEMBER_COUNT: u16 = 3;

/// Cancellation must be observable within this window after the hook that
/// opened the scope started: the 5s deadline fires, and the observation
/// bound sits one second later.
const HOOK_OBSERVE_WINDOW: Duration = Duration::from_secs(6);

/// The wall for waiting on nested scope members to start.
const HOOK_MEMBER_WAIT: Duration = Duration::from_secs(10);

/// The wall for waiting on a turn to reach its terminal update.
const HOOK_TURN_WAIT: Duration = Duration::from_secs(30);

/// Shared observations for one hook-deadline leg: member inferences started
/// and dropped, the recorded scope handle groups, and hook start/finish
/// stamps.
struct HookGateState {
    member_started: tokio::sync::watch::Sender<usize>,
    started_total: AtomicUsize,
    active: AtomicUsize,
    dropped: AtomicUsize,
    groups: Mutex<Vec<Vec<ScopeHandle>>>,
    hook_started: Mutex<Vec<Instant>>,
    hook_finished: Mutex<Vec<Instant>>,
    hook_finish_poisoned: AtomicBool,
}

impl HookGateState {
    fn new() -> Self {
        let (member_started, _) = tokio::sync::watch::channel(0);
        Self {
            member_started,
            started_total: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            groups: Mutex::new(Vec::new()),
            hook_started: Mutex::new(Vec::new()),
            hook_finished: Mutex::new(Vec::new()),
            hook_finish_poisoned: AtomicBool::new(false),
        }
    }
}

/// Stamps the hook finish time when the hook future ends, however the
/// deadline or the owner cancel ends it.
struct HookFutureGuard {
    state: Arc<HookGateState>,
}

impl Drop for HookFutureGuard {
    fn drop(&mut self) {
        match self.state.hook_finished.lock() {
            Ok(mut finished) => {
                finished.push(Instant::now());
            }
            Err(_) => {
                self.state
                    .hook_finish_poisoned
                    .store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Counts one hanging member inference from start to drop.
struct MemberGuard {
    state: Arc<HookGateState>,
}

impl MemberGuard {
    fn start(state: Arc<HookGateState>) -> Self {
        state.active.fetch_add(1, Ordering::SeqCst);
        let started = state.started_total.fetch_add(1, Ordering::SeqCst) + 1;
        state.member_started.send_replace(started);
        Self { state }
    }
}

impl Drop for MemberGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        self.state.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

/// A member inference that never answers: it hangs until its owner drops
/// it, which is the cancellation the gate observes.
struct HangingMember {
    state: Arc<HookGateState>,
}

impl ModelHandler for HangingMember {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let _guard = MemberGuard::start(state);
            pending::<Result<EventStream, ModelError>>().await
        })
    }
}

/// The child inference a hook starts: it opens the scope, holds the member
/// handles, and never finishes on its own.
struct ScopeOwner {
    state: Arc<HookGateState>,
}

impl ModelHandler for ScopeOwner {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let held = hold_scope(&state, &cx).await;
            Ok(held.unwrap_or_else(|cause| failed_stream(&cause)))
        })
    }
}

/// Why the scope owner could not hold its scope. `ModelError` has no variant
/// for any of these, so each keeps its own cause and the handler reports it
/// through `failed_stream`.
#[derive(Debug)]
enum HoldError {
    /// The scope did not open.
    Open(ScopeError),
    /// The scope refused one member inference.
    Member { index: usize, source: ScopeError },
    /// A thread panicked while it held the scope group mutex. The text of
    /// the poison error is kept: its guard is not `Send`.
    GroupsPoisoned(String),
    /// The members did not all start within `HOOK_MEMBER_WAIT`.
    MembersNotStarted(tokio::time::error::Elapsed),
    /// The member-start tracker closed while the owner waited on it.
    TrackerClosed(tokio::sync::watch::error::RecvError),
}

impl fmt::Display for HoldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => write!(formatter, "the scope did not open: {error}"),
            Self::Member { index, source } => {
                write!(formatter, "member {index} was refused: {source}")
            }
            Self::GroupsPoisoned(cause) => {
                write!(formatter, "the scope group mutex is poisoned: {cause}")
            }
            Self::MembersNotStarted(error) => write!(
                formatter,
                "members did not start within {HOOK_MEMBER_WAIT:?}: {error}"
            ),
            Self::TrackerClosed(error) => {
                write!(formatter, "the member-start tracker closed: {error}")
            }
        }
    }
}

impl std::error::Error for HoldError {}

/// Opens the owner's nested scope, records its member handles, waits until
/// every recorded member has started, and then holds the scope until the
/// owner drops this future.
async fn hold_scope(state: &HookGateState, cx: &ModelCx<'_>) -> Result<EventStream, HoldError> {
    let scope = cx
        .scope(ScopeSpec {
            limit: HOOK_MEMBER_COUNT,
            on_error: OnError::Settle,
            budget: Budget::default(),
        })
        .map_err(HoldError::Open)?;
    let handles = (0..usize::from(HOOK_MEMBER_COUNT))
        .map(|index| {
            scope
                .infer(hook_request(
                    HOOK_MEMBER_ROUTE,
                    &format!("held member {index}"),
                ))
                .map_err(|source| HoldError::Member { index, source })
        })
        .collect::<Result<Vec<_>, HoldError>>()?;
    let group = {
        let mut groups = state
            .groups
            .lock()
            .map_err(|poison| HoldError::GroupsPoisoned(poison.to_string()))?;
        groups.push(handles);
        groups.len()
    };
    let target = group * usize::from(HOOK_MEMBER_COUNT);
    let mut started = state.member_started.subscribe();
    while *started.borrow_and_update() < target {
        tokio::time::timeout(HOOK_MEMBER_WAIT, started.changed())
            .await
            .map_err(HoldError::MembersNotStarted)?
            .map_err(HoldError::TrackerClosed)?;
    }
    pending::<Result<EventStream, HoldError>>().await
}

/// The real `before_turn` hook: it starts one trusted child inference on
/// `target` and holds it, so the child's scope stays alive until the hook
/// deadline or the owning turn's cancel ends the hook.
struct DeadlineHook {
    state: Arc<HookGateState>,
    target: &'static str,
}

impl Hook<dal_core::ext::BeforeTurn, Option<String>> for DeadlineHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let state = Arc::clone(&self.state);
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        let request = hook_request(self.target, "hold a scope past the hook deadline");
        Box::pin(async move {
            state
                .hook_started
                .lock()
                .map_err(|_| HookError::Failed {
                    message: "hook start mutex poisoned".into(),
                })?
                .push(Instant::now());
            let _guard = HookFutureGuard {
                state: Arc::clone(&state),
            };
            services
                .infer(&caller, request)
                .await
                .map_err(|_| HookError::Cancelled)?;
            Ok(None)
        })
    }
}

/// One bare model request for the hook-driven inference paths.
fn hook_request(route: &str, system: &str) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::from_id(route),
        system: Arc::from(system),
        tools: Vec::new().into(),
        context: Vec::<ContextItem>::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn hook_caps() -> Caps {
    Caps {
        context_window: Some(200_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: false,
        image_input: false,
        custom_grammar: false,
    }
}

fn hook_model_record(
    id: &'static str,
    state: Arc<HookGateState>,
) -> Result<ModelRecord, dal_core::RegistrationError> {
    let handler: Arc<dyn ModelHandler> = if id == HOOK_MEMBER_ROUTE {
        Arc::new(HangingMember { state })
    } else {
        Arc::new(ScopeOwner { state })
    };
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: hook_caps(),
        handler,
        export: None,
    })
}

/// The Rust-hook extension: hanging members, the scope-owning child model,
/// and the `before_turn` hook that drives it.
fn rust_hook_extension()
-> Result<(Arc<HookGateState>, dal_agent::ext::Extension), dal_core::RegistrationError> {
    let state = Arc::new(HookGateState::new());
    let member = hook_model_record(HOOK_MEMBER_ROUTE, Arc::clone(&state))?;
    let owner = hook_model_record(HOOK_OWNER_ROUTE, Arc::clone(&state))?;
    let extension = ExtensionBuilder::new("hook-gate", "0.1.0", ServiceSet::default())?
        .model(member)
        .model(owner)
        .on_before_turn(DeadlineHook {
            state: Arc::clone(&state),
            target: HOOK_OWNER_ROUTE,
        })
        .build()?;
    Ok((state, extension))
}

/// The scripted-hook extension: the same hanging members, but the hook
/// drives the Starlark plugin's scope-owning model instead of a Rust one.
fn star_hook_extension()
-> Result<(Arc<HookGateState>, dal_agent::ext::Extension), dal_core::RegistrationError> {
    let state = Arc::new(HookGateState::new());
    let member = hook_model_record(HOOK_MEMBER_ROUTE, Arc::clone(&state))?;
    let extension = ExtensionBuilder::new("hook-gate", "0.1.0", ServiceSet::default())?
        .model(member)
        .on_before_turn(DeadlineHook {
            state: Arc::clone(&state),
            target: STAR_HOLD_ROUTE,
        })
        .build()?;
    Ok((state, extension))
}

/// Copies the tracked Starlark fixture into the scratch plugin tree, so the
/// product's real plugin loader registers it.
fn install_hook_plugin(data: &Path) -> support::TestResult<()> {
    let source = support::repo_root().join(format!(
        "dalgona/gates/tests/fixtures/hooks/{STAR_PLUGIN}.star"
    ));
    let directory = data.join("plugins").join(STAR_PLUGIN);
    fs::create_dir_all(&directory)?;
    fs::copy(source, directory.join("plugin.star"))?;
    Ok(())
}

/// Builds the real composed product on `data`, pushes `extension`, grants
/// every registered extension its declared services for `workspace` (the
/// plugin's scope member submissions need the Infer grant), starts the
/// host, and opens one session.
async fn start_hook_host(
    data: &Path,
    workspace: &Path,
    user: &str,
    extension: dal_agent::ext::Extension,
) -> support::TestResult<(dal_agent::Host, dal_agent::Agent)> {
    let mut product = support::build_product(data.to_path_buf(), Some(user))?;
    product.extensions.push(extension);
    for registered in &product.extensions {
        let services = registered.inject().capabilities();
        if services.is_empty() || registered.origin() == dal_core::Origin::Builtin {
            continue;
        }
        let Some(key) = dal_agent::ext::grants::GrantKey::for_extension(registered, workspace)
        else {
            return Err("registered extension has no code identity".into());
        };
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(
                key,
                dal_core::ClientId::new("hook-deadline-gate"),
                dal_core::Timestamp::now(),
            )
            .await?;
    }
    let config = support::host_config(data, user)?;
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::default(),
        cwd: data.to_path_buf(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, config, env).await?;
    let agent = host
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(workspace.to_path_buf())?,
                name: None,
            },
            dal_core::ClientId::new("hook-deadline-gate"),
        )
        .await?;
    Ok((host, agent))
}

/// Waits until `target` member inferences have started on `state`.
async fn wait_hook_members(state: &HookGateState, target: usize) -> support::TestResult<()> {
    let mut started = state.member_started.subscribe();
    let seen = tokio::time::timeout(HOOK_MEMBER_WAIT, async {
        while *started.borrow_and_update() < target {
            started
                .changed()
                .await
                .map_err(|error| format!("member-start tracker stays open: {error}"))?;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    match seen {
        Err(_) => {
            return Err(format!(
                "hook scope members never started ({}/{} within {HOOK_MEMBER_WAIT:?})",
                *started.borrow(),
                target
            )
            .into());
        }
        Ok(Err(error)) => {
            return Err(error);
        }
        Ok(Ok(())) => {}
    }
    Ok(())
}

/// Waits for one turn-end update and requires its stop reason.
async fn wait_hook_turn_end(
    updates: &mut Subscription,
    expected: Stop,
    leg: &str,
) -> support::TestResult<()> {
    let seen = tokio::time::timeout(HOOK_TURN_WAIT, async {
        while let Some(delivery) = updates.next().await {
            if let Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = &update.kind
            {
                return Some(*stop);
            }
        }
        None
    })
    .await;
    match seen {
        Ok(Some(stop)) if stop == expected => Ok(()),
        Ok(Some(stop)) => Err(format!("{leg}: turn ended {stop:?}, expected {expected:?}").into()),
        Ok(None) => Err(format!("{leg}: the update stream ended before the turn ended").into()),
        Err(_) => Err(format!("{leg}: the turn never ended within {HOOK_TURN_WAIT:?}").into()),
    }
}

/// Polls until every started member on `state` has been dropped, which is
/// the consumer-visible shape of "all unfinished handles cancelled". The
/// bound is the last moment the cancellation may be observed.
async fn wait_hook_members_gone(
    state: &HookGateState,
    expected_dropped: usize,
    bound: Instant,
    leg: &str,
) -> support::TestResult<()> {
    while state.active.load(Ordering::SeqCst) != 0
        || state.dropped.load(Ordering::SeqCst) != expected_dropped
    {
        if Instant::now() >= bound {
            return Err(format!(
                "{leg}: hook scope members were not all cancelled by the observation bound: \
                 active={}, dropped={}/{} (the 5s hook deadline did not cancel them)",
                state.active.load(Ordering::SeqCst),
                state.dropped.load(Ordering::SeqCst),
                expected_dropped,
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// The deadline leg's cancellation check: the members must still be running
/// when the 5s hook deadline fires, and gone by the observation bound, so
/// the observed cancellation sits between 5s and 6s after the hook started.
async fn wait_hook_cancelled_at_deadline(
    state: &HookGateState,
    leg: &str,
) -> support::TestResult<()> {
    if state.hook_finish_poisoned.load(Ordering::SeqCst) {
        return Err("hook finish mutex poisoned".into());
    }
    let started = *state
        .hook_started
        .lock()
        .map_err(|_| "hook start mutex")?
        .last()
        .ok_or("the hook ran")?;
    wait_hook_members_gone(
        state,
        usize::from(HOOK_MEMBER_COUNT),
        started + HOOK_OBSERVE_WINDOW,
        leg,
    )
    .await?;
    let observed = started.elapsed();
    assert!(
        observed >= Duration::from_secs(5),
        "{leg}: hook scope members were cancelled {observed:?} after the hook started, \
         before the 5s hook deadline fired"
    );
    Ok(())
}

/// Requires one recorded scope group whose handles all ended `Cancelled`
/// with the typed cancellation error.
fn assert_hook_group_cancelled(
    state: &HookGateState,
    index: usize,
    leg: &str,
) -> support::TestResult<()> {
    let groups = state
        .groups
        .lock()
        .map_err(|_| "scope group mutex")?
        .clone();
    let Some(group) = groups.get(index) else {
        return Err(format!("{leg}: scope group {index} was never recorded").into());
    };
    assert_eq!(
        group.len(),
        usize::from(HOOK_MEMBER_COUNT),
        "{leg}: scope group {index} holds {} handles",
        group.len()
    );
    for handle in group {
        assert_eq!(
            handle.status(),
            ScopeStatus::Cancelled,
            "{leg}: a held scope handle did not end Cancelled"
        );
        assert_eq!(
            handle.error(),
            Some(ScopeError::Cancelled),
            "{leg}: a held scope handle did not carry the typed Cancelled error"
        );
    }
    Ok(())
}

/// Requires the second hook, cancelled with its owning turn, to have
/// finished before its own 5s deadline: the cancel ended it, not the
/// deadline. `started` is the second hook's start stamp.
fn assert_cancelled_hook_finished_before_deadline(
    state: &HookGateState,
    started: Instant,
    leg: &str,
) -> support::TestResult<()> {
    if state.hook_finish_poisoned.load(Ordering::SeqCst) {
        return Err("hook finish mutex poisoned".into());
    }
    let finished = state
        .hook_finished
        .lock()
        .map_err(|_| "hook finish mutex")?;
    let finish_elapsed = finished
        .iter()
        .copied()
        .find(|finished| *finished >= started)
        .ok_or("the cancelled hook never finished")?
        .duration_since(started);
    assert!(
        finish_elapsed < Duration::from_secs(5),
        "{leg}: the cancelled hook ran {finish_elapsed:?}, past its own deadline"
    );
    Ok(())
}

/// The hook-deadline criterion: a real `before_turn` hook starts a child
/// inference whose model opens a scope and holds three member handles past
/// the hook deadline. The deadline is the runtime's own 5s hook budget
/// (`HOOK_DEADLINE_MS`, applied by the hook dispatch); this gate writes no
/// timeout configuration, and the documented `hooks.deadline` TOML key is
/// deliberately absent because no code reads it.
///
/// Two subcases share this one fn. The Rust hook subcase proves the
/// deadline cancels every unfinished member handle between 5s and 6s after
/// the hook started, and that cancelling the owning turn cancels the next
/// nested scope promptly with the typed `Cancelled` error on every handle.
/// The scripted Starlark hook subcase drives the same hook against the
/// plugin's scope-owning model (`fixtures/hooks/{STAR_PLUGIN}.star`,
/// registered through the real plugin loader with its Infer grant explicit)
/// and requires the same 5-6s cancellation. That subcase is expected red
/// today, but for an earlier reason than a missed deadline: the hook drives
/// the scripted route through the trusted `Services::infer` path, whose
/// `RequestDeps` carries no script seam (`Backend::infer` builds it with
/// `script: None`, unlike the turn driver and the in-script model op which
/// pass `Some`). `ScriptModelHandler::run` therefore fails closed at its
/// top-level `script_cx()` gate with "script context is unavailable for
/// this model" before any Starlark executes; the hook maps that to
/// `HookError::Cancelled`, and no member ever starts (0/3). That first
/// failure is host-side, not fixture-side: the fixture's
/// `hold(ctx, request)` signature and its
/// `ctx.scope(limit = 3, on_error = "settle")` / `scope.infer` /
/// `scope.settle()` calls match the `dal` v1 API and the relay/cycle-depth
/// precedent, so there is no `ctx.scope` attribute error to fix in the
/// script, no 60s `CELL_WALL` model-phase cap is ever entered, and no
/// member leaks. The observable signature is a hook that started and
/// finished almost together with zero member starts while the turn still
/// reaches `EndTurn`. The runtime fix is production-side (dal-agent
/// session/script seam, LOCAL owner; not this gate): thread the session
/// script host into hook-driven trusted inference so `script_cx()`
/// resolves. Once the script is entered it runs under its own wall, and the
/// drop-guard ownership still applies on top: finish (and so the scope
/// cancellation) must run on every exit path, including the deadline drop,
/// without detached reaper tasks.
#[test]
fn scope_created_in_hook_is_cancelled_at_hook_deadline() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        rust_hook_deadline_leg().await?;
        star_hook_deadline_leg().await
    });
    // A red leg can leave hook-held scopes behind (the scripted leg fails
    // before any worker starts, so there is no blocked script to drain);
    // bound the runtime teardown instead of hanging the gate on it.
    runtime.shutdown_timeout(Duration::from_secs(15));
    outcome
}

/// The Rust-hook subcase: deadline expiry, then owner-turn cancel.
async fn rust_hook_deadline_leg() -> support::TestResult<()> {
    let leg = "rust hook";
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-hook-deadline-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let fixture = data.join("hook-deadline.jsonl");
    fs::write(
        &fixture,
        format!(
            "{}\n{}\n",
            support::text_step("deadline passed"),
            support::text_step("second turn")
        ),
    )?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[judge]\ngate = \"off\"\n\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let (state, extension) =
        rust_hook_extension().map_err(|error| format!("{leg}: extension: {error}"))?;
    let (host, agent) = start_hook_host(&data, &workspace, &user, extension).await?;
    let mut updates = agent.subscribe(None)?;

    // Deadline leg: the hook holds the scope until the 5s deadline fires.
    let accepted = agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![dal_core::Part::Text {
                text: "open a scope inside the before-turn hook".into(),
            }],
        })
        .await?;
    assert!(
        matches!(accepted, Reply::Accepted { .. }),
        "{leg}: the deadline prompt was not accepted"
    );
    wait_hook_members(&state, usize::from(HOOK_MEMBER_COUNT)).await?;
    wait_hook_cancelled_at_deadline(&state, leg).await?;
    assert_hook_group_cancelled(&state, 0, leg)?;
    wait_hook_turn_end(&mut updates, Stop::EndTurn, leg).await?;

    // Owner-turn cancel leg: a second hook holds a fresh nested scope and
    // the owning turn is cancelled under it.
    let Reply::Accepted { turn, .. } = agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![dal_core::Part::Text {
                text: "open another scope inside the before-turn hook".into(),
            }],
        })
        .await?
    else {
        return Err(format!("{leg}: the second prompt was not accepted").into());
    };
    wait_hook_members(&state, 2 * usize::from(HOOK_MEMBER_COUNT)).await?;
    let cancelled = agent
        .submit(dal_core::Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await?;
    assert!(
        matches!(cancelled, Reply::Done(_)),
        "{leg}: the cancel command did not complete"
    );
    wait_hook_turn_end(&mut updates, Stop::Cancelled, leg).await?;
    let second_started = *state
        .hook_started
        .lock()
        .map_err(|_| "hook start mutex")?
        .last()
        .ok_or("the second hook ran")?;
    wait_hook_members_gone(
        &state,
        2 * usize::from(HOOK_MEMBER_COUNT),
        second_started + HOOK_OBSERVE_WINDOW,
        leg,
    )
    .await?;
    assert_cancelled_hook_finished_before_deadline(&state, second_started, leg)?;
    assert_hook_group_cancelled(&state, 1, leg)?;
    let report = host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "{leg}: shutdown left tasks behind"
    );
    Ok(())
}

/// The scripted-hook subcase: the same `before_turn` hook drives the Starlark
/// plugin's scope-owning model instead of a Rust one, through the real
/// plugin loader, with the plugin's Infer grant granted explicitly.
async fn star_hook_deadline_leg() -> support::TestResult<()> {
    let leg = "scripted hook";
    let root = TestRoot(std::env::temp_dir().join(format!(
        "dalgona-hook-deadline-star-{}",
        uuid::Uuid::now_v7()
    )));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    install_hook_plugin(&data)?;
    let fixture = data.join("hook-deadline-star.jsonl");
    fs::write(
        &fixture,
        format!(
            "{}\n{}\n",
            support::text_step("deadline passed"),
            support::text_step("second turn")
        ),
    )?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\nplugins = [\"{STAR_PLUGIN}\"]\n\n[judge]\ngate = \"off\"\n\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let (state, extension) =
        star_hook_extension().map_err(|error| format!("{leg}: extension: {error}"))?;
    let (host, agent) = start_hook_host(&data, &workspace, &user, extension).await?;
    let mut updates = agent.subscribe(None)?;
    let accepted = agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![dal_core::Part::Text {
                text: "open a scope inside the scripted before-turn hook".into(),
            }],
        })
        .await?;
    assert!(
        matches!(accepted, Reply::Accepted { .. }),
        "{leg}: the deadline prompt was not accepted"
    );
    wait_hook_members(&state, usize::from(HOOK_MEMBER_COUNT)).await?;
    wait_hook_cancelled_at_deadline(&state, leg).await?;
    wait_hook_turn_end(&mut updates, Stop::EndTurn, leg).await?;
    let report = host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "{leg}: shutdown left tasks behind"
    );
    Ok(())
}
