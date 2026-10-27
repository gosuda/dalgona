// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the goal sidecar is durable and private.
//! A created goal is written atomically beside the session with mode 0600,
//! carries the owning session id, and survives a reopen with its counters.
//!
//! The scope gates share this target: fan-out admission is FIFO under a held
//! member boundary and usage rolls up through nested scopes into the outer
//! budget, and a USD-limited scope refuses an unpriced route before
//! admission instead of booking it at zero cost.
use dalgona_gates::support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::{
    Delivery, Env, SessionRef,
    ext::{
        BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
        ScopeError, ScopeStatus, ScopeValue,
    },
};
use dal_core::{
    Budget, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Expect, ModelId,
    ModelRequest, ModelRoute, OnError, Part, Purpose, Reply, RequestParams, ScopeSpec, ServiceSet,
    Stop, ThinkingLevel, UpdateKind, Usage, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use sonic_rs::JsonContainerTrait;
use tokio::sync::watch;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn goal_sidecar_is_durable_and_private_across_reopen() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-goal-gate-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, support::text_step("ready"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
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
                        .expect("registered extension has a code identity"),
                    dal_core::ClientId::new("goal-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let host = support::start_dalgona_with_config(data.clone(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace.clone())?,
                    name: None,
                },
                dal_core::ClientId::new("goal-gate"),
            )
            .await?;
        let session = agent.view(dal_core::PageReq::default())?.session.id;
        let created = run_command(&agent, "goal", "write the parser").await?;
        assert!(
            created.contains("goal g1: active"),
            "unexpected create reply: {created}"
        );
        let goal_file = find_goal_file(&data, &session)?;
        let document = fs::read_to_string(&goal_file)?;
        let value: sonic_rs::Value = sonic_rs::from_str(document.trim_end())?;
        let object = value.as_object().ok_or("goal.json is not an object")?;
        assert_eq!(
            object
                .get(&"session")
                .and_then(sonic_rs::JsonValueTrait::as_str),
            Some(session.to_string().as_str()),
            "the sidecar must name its owning session"
        );
        assert_eq!(
            object.get(&"v").and_then(sonic_rs::JsonValueTrait::as_u64),
            Some(1),
            "the sidecar version must be 1"
        );
        #[cfg(unix)]
        {
            let mode = fs::metadata(&goal_file)?.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the sidecar must be mode 0600, found {mode:o}");
        }

        host.close(session).await?;
        let reopened = host
            .open(
                dal_agent::SessionRef::Resume {
                    key: session.to_string().into(),
                    workspace: dal_core::Workspace::new(workspace)?,
                },
                dal_core::ClientId::new("goal-gate-reopen"),
            )
            .await?;
        let shown = run_command(&reopened, "goal", "").await?;
        assert!(
            shown.contains("write the parser"),
            "the goal did not survive the reopen: {shown}"
        );
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

async fn run_command(
    agent: &dal_agent::Agent,
    name: &str,
    args: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let reply = agent
        .submit(dal_core::Command::Run {
            name: name.into(),
            args: args.into(),
            expected: None,
        })
        .await?;
    let dal_core::Reply::Done(output) = reply else {
        return Err(format!("unexpected command reply: {reply:?}").into());
    };
    let dal_core::Output::Text(text) = output else {
        return Err(format!("unexpected command output: {output:?}").into());
    };
    Ok(text.to_string())
}

fn find_goal_file(
    data: &Path,
    session: &dal_core::SessionId,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let mut stack = vec![data.join("sessions")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "goal.json") {
                return Ok(path);
            }
        }
    }
    Err(format!("no goal.json beside session {session}").into())
}

// === Scope gates: FIFO admission with nested rollup, and unpriced fail-closed ===
//
// The scenario mirrors the verified owner mechanics of `dal-full-06` on the
// composed Dalgona product: a synthetic root model opens the outer scope
// through the public `ModelCx::scope`, one `scope-live/nester` member opens
// the 500-handle inner scope from inside its own model run (so the inner
// ledger is a true child of the outer one), and every member inference is
// held at the model boundary until released, which makes admission order an
// observable fact instead of a status echo. Member responses come from a
// scratch `[providers.scripted]` fixture whose steps are identical, so the
// scripted queue order never matters and every step measures one request,
// one input token, one output token, and $0.001.

const MEMBER_COUNT: usize = 500;
const ADMITTED: usize = 64;
const OWNER_ROUTE: &str = "scope-live/owner";
const NESTER_ROUTE: &str = "scope-live/nester";
const MEMBER_ROUTE: &str = "scope-live/member";
const UNPRICED_ROUTE: &str = "scope-live/unpriced";
const SCRIPTED_ROUTE: &str = "openai/gpt-6-luna";
/// The whole turn, including the 500 sequential release/join cycles.
const TURN_WAIT: Duration = Duration::from_secs(300);
const STEP_WAIT: Duration = Duration::from_secs(30);
const JOIN_WAIT: Duration = Duration::from_secs(120);
const REPLAY_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"member"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":0.001}},{"type":"stop","reason":"end_turn"}]}"#;

/// FIFO boundary progress: which members entered, run, and wait, and whether
/// promotion followed submission order without stalling.
#[derive(Clone, Default)]
struct BoundaryState {
    reached: bool,
    admitted: usize,
    waiting: usize,
    fifo: bool,
    stalled: bool,
}

/// A budget probe outcome: whether the next unit is refused as exhausted and,
/// otherwise, what the unexpected admission was.
#[derive(Clone, Default)]
struct ProbeOutcome {
    exhausted: bool,
    other: Option<String>,
}

/// Whether the nester member finished, with the first fault it reported.
#[derive(Clone, Default)]
struct NesterOutcome {
    done: bool,
    error: Option<String>,
}

/// Whether the priced member was admitted after the unpriced refusal, with
/// the first failure it reported.
#[derive(Clone, Default)]
struct PricedOutcome {
    admitted: bool,
    result: Option<String>,
}

/// What the handlers observed, asserted by the test body after the turn.
#[derive(Clone, Default)]
struct ScopeRun {
    submitted_ok: usize,
    boundary: BoundaryState,
    completed: usize,
    readable: usize,
    cancelled: usize,
    failed: usize,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
    inner_probe: ProbeOutcome,
    nester: NesterOutcome,
    outer_open_error: Option<String>,
    outer_probe: ProbeOutcome,
    unpriced_error: Option<ScopeError>,
    priced: PricedOutcome,
    all_handles: usize,
    priced_input: u64,
    priced_output: u64,
    priced_cost: Option<f64>,
}

/// Per-test coordination: one release flag per held member, the live order
/// of boundary entries, and the finished-run observations. No global state.
struct ScopeControl {
    releases: Vec<watch::Sender<bool>>,
    started: Mutex<Vec<usize>>,
    started_count: watch::Sender<usize>,
    run: Mutex<Option<ScopeRun>>,
}

impl ScopeControl {
    fn new(releases: usize) -> Self {
        Self {
            releases: (0..releases).map(|_| watch::channel(false).0).collect(),
            started: Mutex::new(Vec::new()),
            started_count: watch::channel(0).0,
            run: Mutex::new(None),
        }
    }

    fn lock_run(&self, apply: impl FnOnce(&mut ScopeRun)) -> Result<(), ModelError> {
        let mut run = self.run.lock().map_err(|_| ModelError::PrivateRounds)?;
        apply(run.get_or_insert_with(ScopeRun::default));
        Ok(())
    }
}

/// The composed product's root model: opens the outer scope, joins the
/// nester that drives the whole 500-member inner scenario, then probes the
/// outer budget so only the rolled-up nested spend can have exhausted it.
struct OwnerModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for OwnerModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            // The outer budget sits just above the nested fan-out: 500 inner
            // units roll up to $0.50, the nester's own response adds $0.001,
            // and $0.5005 is crossed only by that rolled-up total. It is
            // never crossed mid-fan-out (peak $0.50), so the exhaustion is
            // deterministic and charge-attributed.
            let outer = match cx.scope(ScopeSpec {
                limit: 2,
                on_error: OnError::Settle,
                budget: Budget {
                    usd: Some(0.5005),
                    ..Budget::default()
                },
            }) {
                Ok(outer) => outer,
                Err(error) => {
                    control.lock_run(|run| run.outer_open_error = Some(format!("{error}")))?;
                    return Ok(text_stream("outer scope refused"));
                }
            };
            match outer.infer(model_request(NESTER_ROUTE, "root")) {
                Ok(handle) => {
                    let joined = tokio::time::timeout(STEP_WAIT * 8, handle.result()).await;
                    let done = match joined {
                        Ok(Ok(ScopeValue::Inference(_))) => true,
                        Ok(Ok(value)) => {
                            nester_fault(&control, format!("unexpected nester value {value:?}"))?;
                            false
                        }
                        Ok(Err(error)) => {
                            nester_fault(&control, format!("nester failed: {error}"))?;
                            false
                        }
                        Err(_) => {
                            nester_fault(&control, "the nester join timed out".into())?;
                            false
                        }
                    };
                    control.lock_run(|run| run.nester.done = done)?;
                }
                Err(error) => {
                    nester_fault(&control, format!("nester infer refused: {error}"))?;
                }
            }
            let probe = outer.infer(model_request(MEMBER_ROUTE, "probe"));
            control.lock_run(|run| match probe {
                Err(ScopeError::Exhausted) => run.outer_probe.exhausted = true,
                Err(other) => run.outer_probe.other = Some(format!("{other}")),
                Ok(_) => {
                    run.outer_probe.other =
                        Some("the outer scope admitted a unit after the nested fan-out".into());
                }
            })?;
            Ok(text_stream("outer scope settled"))
        })
    }
}

/// The outer scope's single member: from inside its own model run it opens
/// the 500-handle inner scope (a true nested ledger), holds the FIFO
/// boundary, releases one member at a time, and accounts the rollup.
struct NesterModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for NesterModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let scope = match cx.scope(ScopeSpec {
                limit: 64,
                on_error: OnError::Settle,
                budget: Budget {
                    requests: Some(500),
                    input_tokens: Some(500),
                    output_tokens: Some(500),
                    wall: Some(Duration::from_secs(300)),
                    usd: Some(1.00),
                },
            }) {
                Ok(scope) => scope,
                Err(error) => {
                    control.lock_run(|run| {
                        run.nester.error = Some(format!("inner scope refused: {error}"));
                    })?;
                    return Ok(text_stream("inner scope refused"));
                }
            };
            let mut handles = Vec::with_capacity(MEMBER_COUNT);
            for index in 0..MEMBER_COUNT {
                match scope.infer(model_request(MEMBER_ROUTE, &index.to_string())) {
                    Ok(handle) => handles.push(handle),
                    Err(error) => control.lock_run(|run| {
                        run.nester.error = Some(format!("submission {index} refused: {error}"));
                    })?,
                }
            }
            control.lock_run(|run| run.submitted_ok = handles.len())?;
            if handles.len() == MEMBER_COUNT {
                run_fifo_protocol(&control, &mut handles).await?;
            }
            let Ok(all) = tokio::time::timeout(JOIN_WAIT, scope.all()).await else {
                control.lock_run(|run| {
                    run.nester.error = Some("the inner scope never settled".into());
                })?;
                return Ok(text_stream("inner scope did not settle"));
            };
            // The cost fold starts at a measured zero so the first priced
            // unit accumulates instead of reading the empty total as an
            // unknown; any unit without a cost still poisons the total.
            let mut state = ScopeRun {
                cost_usd: Some(0.0),
                ..ScopeRun::default()
            };
            for handle in &all {
                if handle.status() == ScopeStatus::Done {
                    state.completed += 1;
                }
                match handle.error() {
                    Some(ScopeError::Cancelled) => state.cancelled += 1,
                    Some(_) => state.failed += 1,
                    None => {}
                }
                let usage = handle.usage();
                state.requests += 1;
                state.input_tokens += usage.input_tokens;
                state.output_tokens += usage.output_tokens;
                state.cost_usd = match (state.cost_usd, usage.cost_usd) {
                    (Some(total), Some(part)) => Some(total + part),
                    _ => None,
                };
                // A finished handle stays readable until the scope drops.
                if matches!(
                    tokio::time::timeout(STEP_WAIT, handle.result()).await,
                    Ok(Ok(ScopeValue::Inference(_)))
                ) {
                    state.readable += 1;
                }
            }
            // The budget is saturated exactly: a 501st unit must be refused,
            // so nothing was double-counted through the nesting and nothing
            // slipped past the limits.
            match scope.infer(model_request(MEMBER_ROUTE, "probe")) {
                Err(ScopeError::Exhausted) => state.inner_probe.exhausted = true,
                Err(other) => state.inner_probe.other = Some(format!("{other}")),
                Ok(_) => {
                    state.inner_probe.other = Some("the inner scope admitted a 501st unit".into());
                }
            }
            control.lock_run(|run| {
                run.completed = state.completed;
                run.readable = state.readable;
                run.cancelled = state.cancelled;
                run.failed = state.failed;
                run.requests = state.requests;
                run.input_tokens = state.input_tokens;
                run.output_tokens = state.output_tokens;
                run.cost_usd = state.cost_usd;
                run.inner_probe.exhausted = state.inner_probe.exhausted;
                run.inner_probe.other.clone_from(&state.inner_probe.other);
            })?;
            // The closing response carries the nester's own measured usage;
            // it is the charge that pushes the rolled-up total over the
            // outer USD budget.
            Ok(text_stream("inner scope settled"))
        })
    }
}

/// Boundary fill, FIFO promotion, and the drain of the initially admitted
/// members. Every wait is bounded; every claim is recorded, never assumed.
async fn run_fifo_protocol(
    control: &ScopeControl,
    handles: &mut [dal_agent::ext::ScopeHandle],
) -> Result<(), ModelError> {
    let mut started = control.started_count.subscribe();
    if !wait_for_started(&mut started, ADMITTED, STEP_WAIT).await {
        control.lock_run(|run| {
            run.nester.error = Some("fewer than 64 members reached the boundary".into());
        })?;
        return Ok(());
    }
    let admitted = handles[..ADMITTED]
        .iter()
        .filter(|handle| handle.status() == ScopeStatus::Running)
        .count();
    let waiting = handles[ADMITTED..]
        .iter()
        .filter(|handle| handle.status() == ScopeStatus::Pending)
        .count();
    // Independent evidence: the order in which member handlers actually
    // entered, not the statuses they report about themselves.
    let entered = control
        .started
        .lock()
        .map_err(|_| ModelError::PrivateRounds)?
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let mut fifo = entered == (0..ADMITTED).collect::<BTreeSet<_>>();
    let mut stalled = false;
    control.releases[0].send_replace(true);
    stalled |= !joins_as_inference(&handles[0], STEP_WAIT).await;
    for index in ADMITTED..MEMBER_COUNT {
        if stalled {
            break;
        }
        if !wait_for_started(&mut started, index + 1, STEP_WAIT).await {
            fifo = false;
            stalled = true;
            break;
        }
        fifo &= control
            .started
            .lock()
            .map_err(|_| ModelError::PrivateRounds)?
            .contains(&index);
        fifo &= handles[index].status() == ScopeStatus::Running;
        fifo &= handles[index + 1..]
            .iter()
            .all(|handle| handle.status() == ScopeStatus::Pending);
        control.releases[index].send_replace(true);
        stalled |= !joins_as_inference(&handles[index], STEP_WAIT).await;
    }
    for (index, handle) in handles.iter().enumerate().take(ADMITTED).skip(1) {
        control.releases[index].send_replace(true);
        let _ = tokio::time::timeout(STEP_WAIT, handle.result()).await;
    }
    control.lock_run(|run| {
        run.boundary.reached = true;
        run.boundary.admitted = admitted;
        run.boundary.waiting = waiting;
        run.boundary.fifo = fifo;
        run.boundary.stalled = stalled;
    })?;
    Ok(())
}

/// The root model of the unpriced gate: a USD-limited scope must refuse an
/// unpriced route with the typed error before admission, then keep working.
struct UnpricedOwnerModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for UnpricedOwnerModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let scope = match cx.scope(ScopeSpec {
                limit: 4,
                on_error: OnError::Settle,
                budget: Budget {
                    requests: Some(1),
                    usd: Some(1.0),
                    ..Budget::default()
                },
            }) {
                Ok(scope) => scope,
                Err(error) => {
                    control.lock_run(|run| {
                        run.nester.error = Some(format!("scope refused: {error}"));
                    })?;
                    return Ok(text_stream("scope refused"));
                }
            };
            control.releases[0].send_replace(true);
            let refused = scope.infer(model_request(UNPRICED_ROUTE, "unpriced"));
            control.lock_run(|run| run.unpriced_error = refused.err())?;
            // The refusal consumed no admission: the one-request budget still
            // had room for a priced member.
            match scope.infer(model_request(MEMBER_ROUTE, "0")) {
                Ok(handle) => {
                    control.lock_run(|run| run.priced.admitted = true)?;
                    match tokio::time::timeout(STEP_WAIT, handle.result()).await {
                        Ok(Ok(ScopeValue::Inference(_))) => {
                            let usage = handle.usage();
                            control.lock_run(|run| {
                                run.priced_input = usage.input_tokens;
                                run.priced_output = usage.output_tokens;
                                run.priced_cost = usage.cost_usd;
                            })?;
                        }
                        Ok(Err(error)) => {
                            control.lock_run(|run| run.priced.result = Some(format!("{error}")))?;
                        }
                        Ok(Ok(value)) => {
                            control.lock_run(|run| {
                                run.priced.result = Some(format!("unexpected value {value:?}"));
                            })?;
                        }
                        Err(_) => {
                            control.lock_run(|run| {
                                run.priced.result = Some("the priced join timed out".into());
                            })?;
                        }
                    }
                }
                Err(error) => {
                    control.lock_run(|run| {
                        run.priced.admitted = false;
                        run.priced.result = Some(format!("{error}"));
                    })?;
                }
            }
            // The refused route created no handle: the scope holds exactly
            // the one priced member and nothing absorbed as zero cost.
            let all_handles = match tokio::time::timeout(STEP_WAIT, scope.all()).await {
                Ok(handles) => handles.len(),
                Err(_) => usize::MAX,
            };
            control.lock_run(|run| run.all_handles = all_handles)?;
            Ok(text_stream("unpriced probe settled"))
        })
    }
}

/// Every member inference is held here until its release flag flips, so the
/// admitted set and the promotion order are observable facts. The response
/// comes from the scripted fixture via a real forward.
struct HeldMemberModel {
    control: Arc<ScopeControl>,
}

impl ModelHandler for HeldMemberModel {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let control = Arc::clone(&self.control);
        Box::pin(async move {
            let index = request
                .system
                .parse::<usize>()
                .map_err(|_| ModelError::PrivateRounds)?;
            {
                let mut started = control
                    .started
                    .lock()
                    .map_err(|_| ModelError::PrivateRounds)?;
                started.push(index);
                control.started_count.send_replace(started.len());
            }
            let mut release = control.releases[index].subscribe();
            while !*release.borrow_and_update() {
                match tokio::time::timeout(STEP_WAIT, release.changed()).await {
                    Ok(Ok(())) => {}
                    _ => return Err(ModelError::PrivateRounds),
                }
            }
            request.model = ModelRoute::from_id(SCRIPTED_ROUTE);
            request.system = Arc::from("");
            cx.forward(request, &[]).await
        })
    }
}

async fn wait_for_started(
    started: &mut watch::Receiver<usize>,
    target: usize,
    wait: Duration,
) -> bool {
    while *started.borrow_and_update() < target {
        match tokio::time::timeout(wait, started.changed()).await {
            Ok(Ok(())) => {}
            _ => return false,
        }
    }
    true
}

async fn joins_as_inference(handle: &dal_agent::ext::ScopeHandle, wait: Duration) -> bool {
    matches!(
        tokio::time::timeout(wait, handle.result()).await,
        Ok(Ok(ScopeValue::Inference(_)))
    )
}

fn model_request(route: &str, system: &str) -> ModelRequest {
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

fn stream_of(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
}

/// One text response carrying the measured unit: one request, one input
/// token, one output token, $0.001.
fn text_stream(text: &str) -> EventStream {
    stream_of(vec![
        Ok(StreamEvent::TextDelta {
            text: text.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage {
            usage: measured_usage(),
        }),
        Ok(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ])
}

fn measured_usage() -> Usage {
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

fn gate_caps() -> Caps {
    Caps {
        context_window: Some(200_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

fn nester_fault(control: &ScopeControl, fault: String) -> Result<(), ModelError> {
    control.lock_run(|run| run.nester.error = Some(fault))
}

fn fanout_extension(
    control: &Arc<ScopeControl>,
) -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    ExtensionBuilder::new("scope-live", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse(OWNER_ROUTE)?,
            caps: gate_caps(),
            handler: Arc::new(OwnerModel {
                control: Arc::clone(control),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse(NESTER_ROUTE)?,
            caps: gate_caps(),
            handler: Arc::new(NesterModel {
                control: Arc::clone(control),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse(MEMBER_ROUTE)?,
            caps: gate_caps(),
            handler: Arc::new(HeldMemberModel {
                control: Arc::clone(control),
            }),
            export: None,
        })
        .build()
}

fn unpriced_extension(
    control: &Arc<ScopeControl>,
) -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    ExtensionBuilder::new("scope-live", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse(OWNER_ROUTE)?,
            caps: gate_caps(),
            handler: Arc::new(UnpricedOwnerModel {
                control: Arc::clone(control),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse(MEMBER_ROUTE)?,
            caps: gate_caps(),
            handler: Arc::new(HeldMemberModel {
                control: Arc::clone(control),
            }),
            export: None,
        })
        .build()
}

fn price_block(route: &str) -> String {
    format!(
        "[prices.\"{route}\"]\ninput = 1.0\ncached_input = 0.0\noutput = 1.0\nreasoning = 0.0\n"
    )
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the 500-member nested scenario is one deliberate end-to-end walkthrough"
)]
fn scope_admission_is_fifo_and_usage_rolls_up_through_nested_scopes() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-scope-gate-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let fixture = data.join("scope-steps.jsonl");
    let mut script = String::with_capacity((REPLAY_STEP.len() + 1) * MEMBER_COUNT);
    for _ in 0..MEMBER_COUNT {
        script.push_str(REPLAY_STEP);
        script.push('\n');
    }
    fs::write(&fixture, script)?;
    let user = format!(
        "model = \"{OWNER_ROUTE}\"\n\n[providers.scripted]\nfixture = {fixture:?}\n\n{}{}",
        price_block(MEMBER_ROUTE),
        price_block(NESTER_ROUTE),
    );
    let control = Arc::new(ScopeControl::new(MEMBER_COUNT));
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
                        .expect("registered extension has a code identity"),
                    ClientId::new("scope-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let mut product = support::build_product(data.clone(), Some(&user))?;
        product.extensions.push(fanout_extension(&control)?);
        let factory = dalgona::product();
        let config = Config::load(ConfigProduct::Dalgona, &data, factory.defaults, Some(&user))?;
        let env = Env {
            vars: BTreeMap::default(),
            cwd: data.clone(),
            sandbox_helper: None,
        };
        let host = dal_agent::Host::start(product, config, env).await?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace)?,
                    name: None,
                },
                ClientId::new("scope-gate"),
            )
            .await?;
        let mut updates = agent.subscribe(None)?;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "run the scoped fan-out".into(),
                }],
            })
            .await?;
        assert!(matches!(reply, Reply::Accepted { .. }));
        let ended = tokio::time::timeout(TURN_WAIT, async {
            while let Some(delivery) = updates.next().await {
                if let Delivery::Update(update) = delivery
                    && matches!(
                        &update.kind,
                        UpdateKind::TurnEnded {
                            stop: Stop::EndTurn,
                            ..
                        }
                    )
                {
                    return true;
                }
            }
            false
        })
        .await?;
        assert!(ended, "the owner turn reached a terminal update");
        let run = control
            .run
            .lock()
            .expect("run mutex")
            .clone()
            .expect("the handlers recorded the scope run");
        assert_eq!(
            run.submitted_ok, MEMBER_COUNT,
            "every member inference is admitted into the nested scope: {:?}",
            run.nester.error
        );
        assert!(
            run.boundary.reached && !run.boundary.stalled,
            "the held boundary filled and every release joined"
        );
        assert_eq!(run.boundary.admitted, ADMITTED, "the first 64 members run");
        assert_eq!(
            run.boundary.waiting,
            MEMBER_COUNT - ADMITTED,
            "the remaining 436 members wait in FIFO order"
        );
        assert!(
            run.boundary.fifo,
            "promotion follows submission order: started set and statuses disagreed"
        );
        assert_eq!(
            run.completed + run.cancelled + run.failed,
            MEMBER_COUNT,
            "every handle reaches a terminal state"
        );
        assert_eq!(
            run.completed, MEMBER_COUNT,
            "all 500 members succeed with no cancelled ({}) or failed ({}) handle",
            run.cancelled, run.failed
        );
        assert_eq!(run.readable, MEMBER_COUNT, "results stay readable");
        assert_eq!(run.requests, 500);
        assert_eq!(run.input_tokens, 500);
        assert_eq!(run.output_tokens, 500);
        let cost = run.cost_usd.expect("every unit reports a known cost");
        assert!(
            (cost - 0.50).abs() < 1e-9,
            "usage must sum to exactly $0.50, found {cost}"
        );
        assert!(
            run.inner_probe.exhausted,
            "the inner budget is exactly saturated: {:?}",
            run.inner_probe.other
        );
        assert!(
            run.nester.done,
            "the nester member completes: {:?}",
            run.nester.error
        );
        assert!(
            run.outer_probe.exhausted,
            "the nested spend must exhaust the outer USD budget: {:?}",
            run.outer_probe
                .other
                .as_deref()
                .or(run.outer_open_error.as_deref())
        );
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

/// Grants every non-builtin extension in `product` a code identity for
/// `client`, so the gate host starts from the same grants the product
/// under test would carry.
async fn grant_extensions(
    product: &dal_agent::Product,
    data: &Path,
    workspace: &Path,
    client: &str,
) -> support::TestResult<()> {
    for extension in &product.extensions {
        let services = extension.inject().capabilities();
        if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
            continue;
        }
        let Some(key) = dal_agent::ext::grants::GrantKey::for_extension(extension, workspace)
        else {
            return Err("registered extension has a code identity".into());
        };
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(key, ClientId::new(client), dal_core::Timestamp::now())
            .await?;
    }
    Ok(())
}

/// Waits for the owner turn to reach a terminal update, bounded by `wait`.
async fn wait_for_turn_end(
    updates: &mut dal_agent::Subscription,
    wait: Duration,
) -> support::TestResult<bool> {
    Ok(tokio::time::timeout(wait, async {
        while let Some(delivery) = updates.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(
                    &update.kind,
                    UpdateKind::TurnEnded {
                        stop: Stop::EndTurn,
                        ..
                    }
                )
            {
                return true;
            }
        }
        false
    })
    .await?)
}

#[test]
fn unpriced_usd_limited_handle_fails_closed() -> support::TestResult<()> {
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-unpriced-gate-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let fixture = data.join("unpriced-step.jsonl");
    fs::write(&fixture, format!("{REPLAY_STEP}\n"))?;
    let user = format!(
        "model = \"{OWNER_ROUTE}\"\n\n[providers.scripted]\nfixture = {fixture:?}\n\n{}",
        price_block(MEMBER_ROUTE),
    );
    let control = Arc::new(ScopeControl::new(1));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let product = support::build_product(data.clone(), Some(&user))?;
        grant_extensions(&product, &data, &workspace, "unpriced-gate").await?;
        drop(product);
        let mut product = support::build_product(data.clone(), Some(&user))?;
        product.extensions.push(unpriced_extension(&control)?);
        let factory = dalgona::product();
        let config = Config::load(ConfigProduct::Dalgona, &data, factory.defaults, Some(&user))?;
        let env = Env {
            vars: BTreeMap::default(),
            cwd: data.clone(),
            sandbox_helper: None,
        };
        let host = dal_agent::Host::start(product, config, env).await?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace)?,
                    name: None,
                },
                ClientId::new("unpriced-gate"),
            )
            .await?;
        let mut updates = agent.subscribe(None)?;
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "probe the unpriced route".into(),
                }],
            })
            .await?;
        assert!(matches!(reply, Reply::Accepted { .. }));
        let ended = wait_for_turn_end(&mut updates, TURN_WAIT).await?;
        assert!(ended, "the owner turn reached a terminal update");
        let run = control
            .run
            .lock()
            .expect("run mutex")
            .clone()
            .expect("the handler recorded the unpriced probe");
        let error = run
            .unpriced_error
            .as_ref()
            .expect("the unpriced route must be refused, never admitted");
        let ScopeError::UnpricedModel { model } = error else {
            panic!("expected the typed UnpricedModel refusal, got {error:?}");
        };
        assert_eq!(model.as_ref(), UNPRICED_ROUTE);
        assert!(
            run.priced.admitted && run.priced.result.is_none(),
            "the scope stayed usable: the refusal happened before admission: {:?}",
            run.priced.result
        );
        assert_eq!(
            run.all_handles, 1,
            "the refused route created no handle: only the priced member exists"
        );
        assert_eq!(run.priced_input, 1);
        assert_eq!(run.priced_output, 1);
        assert_eq!(
            run.priced_cost,
            Some(0.001),
            "the failure was never absorbed as a zero-cost unit"
        );
        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
