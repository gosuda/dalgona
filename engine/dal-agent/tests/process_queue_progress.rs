#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "integration tests signal their own children with kill(1)"
)]

//! Session process cap: while one session already has 200 live children,
//! every further `run` call queues in arrival order and its keyed
//! `ToolProgress` row reads exactly `waiting for a process slot (<n> queued)`
//! with the live queue length. A waiter that leaves (dropped or cancelled)
//! never spawns and shrinks every other waiter's count; each freed slot starts
//! exactly the head waiter; every call finishes once.
//!
//! The tests drive the real extension `run` service through `Host`, a
//! registered tool, and a real session subscription, always at the real
//! 200-process cap.
//!
//! * `two_hundred_ten_sleep_one_runs_queue_ten_and_all_finish` is the named
//!   acceptance workload: 210 concurrent `sleep 1` calls. Each real child
//!   holds its start on the fixture's `release` marker, so the 200-deep cap
//!   is provably full while the 10 over the cap queue; after release every
//!   child runs the same measured one-second sleep.
//! * Two tests cover the transitions that workload cannot pin down by timing
//!   alone. Their children block on `sleep 3600` and the test releases them
//!   one by one with `kill`; waiters join one at a time (the probe waits for
//!   a `go-<n>` file), which fixes their arrival order.
//! * `the_exec_door_...` reaches the other spawn door: an exec-class
//!   registered tool that runs `authorize` + `spawn` itself, the same calls
//!   the `exec` tool makes. A model turn runs exec-class calls one at a time,
//!   so this probe fills the cap with one call's children and queues the next
//!   authorization behind them.
//!
//! Only one burst runs at a time: each holds up to 200 real children.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{
    Agent, Delivery, Env, Host, Product, ServiceError, SessionRef, SpawnOpts, Subscription,
    ToolError,
};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    CallId, CancelScope, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part,
    Preview, RawJson, RunOutput, RunRequest, Stop, ToolClass, ToolSpec, TurnId, UpdateKind,
    Visibility, Workspace,
};
use futures::stream::{FuturesUnordered, StreamExt as _};

/// The per-session active-child cap the host enforces.
const CAP: usize = 200;
/// The acceptance load: 210 concurrent calls, 10 of them over the cap.
const LOAD: usize = 210;
/// The waiter that leaves the queue while it waits (queue position 6).
const ABANDONED: usize = 205;
/// Bound for every wait on the host.
const WAIT: Duration = Duration::from_secs(60);
/// Longest single wait on the subscription before the files are re-read.
const POLL: Duration = Duration::from_millis(50);
/// The progress prefix of a call waiting for a process slot.
const WAITING_PREFIX: &str = "waiting for a process slot (";

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"queue-call\",\"name\":\"fixture__queue\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"queue-call\",\"name\":\"fixture__queue\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Each burst holds 200 real children; run them one at a time.
static ONE_BURST_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The exact progress text of a call waiting behind `queued` calls.
fn waiting(queued: usize) -> String {
    format!("{WAITING_PREFIX}{queued} queued)")
}

/// The child script: record its pid (newline last, so a reader never sees a
/// partial write), then become a sleeper only a signal can end.
fn child_script(index: usize) -> String {
    format!("echo $$ > pid-{index}\nexec sleep 3600\n")
}

/// The pid a started child recorded, once its line is complete.
///
/// Returns `Ok(None)` while the file is missing or its line is still
/// incomplete (no trailing newline): both are the documented pending state
/// for polling. Any other I/O error, or a complete line that does not parse,
/// is an `Err` so callers fail clearly instead of pretending nothing started.
fn pid_of(workspace: &Path, index: usize) -> std::io::Result<Option<u32>> {
    let path = workspace.join(format!("pid-{index}"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(line) = text.strip_suffix('\n') else {
        return Ok(None);
    };
    match line.parse() {
        Ok(pid) => Ok(Some(pid)),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "pid file {} has a complete but unparsable line",
                path.display()
            ),
        )),
    }
}

/// Whether `pid-<index>` holds a complete pid line, propagating unrelated
/// I/O or parse errors instead of pretending the child never started.
fn has_pid(workspace: &Path, index: usize) -> Result<bool, String> {
    pid_of(workspace, index)
        .map(|pid| pid.is_some())
        .map_err(|error| format!("cannot read pid-{index}: {error}"))
}

/// Lines in the acceptance `pids` log. `NotFound` means no child logged yet
/// (the documented pending state, counted as zero); any other I/O error is
/// returned so the wait fails clearly instead of stalling on zero.
fn count_pids(workspace: &Path) -> std::io::Result<usize> {
    match std::fs::read_to_string(workspace.join("pids")) {
        Ok(text) => Ok(text.lines().count()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// How many of the burst's children have started.
fn started(workspace: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    for index in 0..LOAD {
        if pid_of(workspace, index)?.is_some() {
            count += 1;
        }
    }
    Ok(count)
}

fn mark(workspace: &Path, name: &str) -> std::io::Result<()> {
    std::fs::write(workspace.join(name), b"")?;
    Ok(())
}

fn signal(pid: u32, which: &str) {
    let _ = std::process::Command::new("kill")
        .args([which, &pid.to_string()])
        .status();
}

/// Ends child `index` the way a user ends a long job: with SIGTERM.
fn end_child(workspace: &Path, index: usize) -> std::io::Result<()> {
    let Some(pid) = pid_of(workspace, index)? else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("the child to end has not started: pid-{index} is missing"),
        ));
    };
    signal(pid, "-TERM");
    Ok(())
}

/// Reaps every child a failed run left behind: a red run must not leave
/// hour-long sleepers.
struct Reaper {
    workspace: PathBuf,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for index in 0..LOAD {
            let Ok(Some(pid)) = pid_of(&self.workspace, index) else {
                continue;
            };
            let proc_entry = PathBuf::from(format!("/proc/{pid}/cmdline"));
            let ours = match std::fs::read(&proc_entry) {
                Ok(bytes) => bytes.starts_with(b"sleep"),
                Err(_) => !Path::new("/proc/self").exists(),
            };
            if ours {
                signal(pid, "-9");
            }
        }
    }
}

/// Polls for `path` so the probe can be steered from the test.
async fn wait_for_file(path: &Path) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

fn describe(output: &Result<RunOutput, ServiceError>) -> String {
    match output {
        Ok(output) => format!("{:?}", output.status),
        Err(error) => format!("error: {error}"),
    }
}

/// The child script of the acceptance workload: bracket `sleep 1` with an
/// append-only start and end line (one short `O_APPEND` write each, so lines
/// never tear) and record the shell pid.
///
/// Each real child also parks on the fixture's `release` marker before its
/// measured one-second work, so the first 200 admitted slots are provably
/// held while all 210 run attempts arrive and the 10 over the cap queue.
fn acceptance_script() -> String {
    String::from(
        "echo S >> events\necho $$ >> pids\n\
         until [ -f release ]; do sleep 0.05; done\n\
         sleep 1\necho E >> events\n",
    )
}

/// The most children the `events` log shows alive at once.
fn peak_active(workspace: &Path) -> std::io::Result<usize> {
    let text = std::fs::read_to_string(workspace.join("events"))?;
    let mut active = 0_usize;
    let mut peak = 0_usize;
    for line in text.lines() {
        match line {
            "S" => {
                active += 1;
                peak = peak.max(active);
            }
            "E" => active = active.saturating_sub(1),
            _ => {}
        }
    }
    Ok(peak)
}

/// How the probe launches its burst.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// All 210 calls at once, each running `sleep 1`.
    Acceptance,
    /// 200 calls at once, then one waiter per `go-<n>` file, each running a
    /// sleeper only `kill` ends; call 205 is dropped while it waits once the
    /// test creates `abandon-205`.
    Gated,
    /// An exec-class tool: 202 sequential `authorize` + `spawn` pairs from one
    /// call, each child reaped concurrently so its slot frees when it exits.
    Exec,
}

/// The exec door: what the `exec` tool does per child, repeated. Children
/// print nothing, so the call's keyed progress row is only ever a waiting
/// row. Each spawned child is moved into a future that waits for it, because
/// a slot frees when its child is reaped, not when it merely exits.
async fn exec_door(mut cx: ToolCx<'_>) -> ToolOutcome {
    let workspace = cx.workspace().as_path().to_path_buf();
    let cancel = cx.cancel().clone();
    let mut reaping = FuturesUnordered::new();
    for index in 0..CAP + 2 {
        let preview = Preview {
            title: "queue-probe".into(),
            body: format!("child {index}").into(),
            digest: None,
        };
        let authorized = {
            let authorize = cx.authorize(preview);
            tokio::pin!(authorize);
            loop {
                tokio::select! {
                    result = &mut authorize => break result,
                    Some(()) = reaping.next() => {}
                }
            }
        };
        let approved = match authorized {
            Ok(approved) => approved,
            Err(reason) => return ToolOutcome::Err(ToolError::Denied(reason)),
        };
        let argv = [
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from(child_script(index)),
        ];
        let opts = SpawnOpts {
            cwd: workspace.clone(),
            timeout: None,
            env: Vec::new(),
            stdout_prefix_limit: 0,
        };
        let mut proc = match cx.spawn(&argv, opts, approved) {
            Ok(proc) => proc,
            Err(error) => return ToolOutcome::Err(error),
        };
        let cancel = cancel.clone();
        reaping.push(async move {
            let _ = proc.wait(&cancel).await;
        });
    }
    while reaping.next().await.is_some() {}
    ToolOutcome::Ok(Box::new(ToolOutput::from_text("exec door finished")))
}

type RunResults = Arc<std::sync::Mutex<Vec<(usize, String)>>>;

/// A model-visible tool that fires its burst through the extension `run`
/// service, or through the exec door in [`Mode::Exec`].
struct QueueProbe {
    name: Name,
    spec: Arc<ToolSpec>,
    mode: Mode,
    results: RunResults,
}

impl Tool for QueueProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(match self.mode {
            Mode::Exec => ToolClass::Exec {
                read_only: false,
                grant: None,
            },
            Mode::Acceptance | Mode::Gated => ToolClass::Other,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            if self.mode == Mode::Exec {
                return exec_door(cx).await;
            }
            let services = cx.services();
            let caller = cx.caller().clone();
            let workspace = cx.workspace().as_path().to_path_buf();
            let results = Arc::clone(&self.results);
            let mode = self.mode;
            let start = |index: usize| {
                let services = Arc::clone(&services);
                let caller = caller.clone();
                let workspace = workspace.clone();
                let results = Arc::clone(&results);
                async move {
                    let request = RunRequest {
                        argv: vec![
                            OsString::from("sh"),
                            OsString::from("-c"),
                            OsString::from(match mode {
                                Mode::Acceptance | Mode::Exec => acceptance_script(),
                                Mode::Gated => child_script(index),
                            }),
                        ],
                        cwd: None,
                        stdin: None,
                        timeout: None,
                        env: Vec::new(),
                        stdout_prefix_limit: 4096,
                    };
                    let mut run = services.run(&caller, request);
                    let text = if mode == Mode::Gated && index == ABANDONED {
                        let abandon = workspace.join(format!("abandon-{index}"));
                        tokio::select! {
                            output = &mut run => describe(&output),
                            _ = wait_for_file(&abandon) => String::from("abandoned"),
                        }
                    } else {
                        describe(&(&mut run).await)
                    };
                    drop(run);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((index, text));
                }
            };
            // The call's cancellation ends this probe: a cancelled turn must
            // end even though the remaining gates never open.
            let cancelled = cx.cancel().clone();
            // The probe owns every run task; dropping the set on the cancel
            // path aborts them, so no run outlives the tool call that owns it.
            let mut running = tokio::task::JoinSet::new();
            let gate_from = match mode {
                Mode::Acceptance | Mode::Exec => LOAD,
                Mode::Gated => CAP,
            };
            for index in 0..LOAD {
                if index >= gate_from {
                    // The runs already in flight keep being driven while the
                    // test decides when the next waiter arrives.
                    let go = workspace.join(format!("go-{index}"));
                    let mut gate = Box::pin(wait_for_file(&go));
                    loop {
                        tokio::select! {
                            _ = &mut gate => break,
                            Some(_) = running.join_next() => {}
                            () = cancelled.cancelled() => {
                                return ToolOutcome::Interrupted;
                            }
                        }
                    }
                }
                // Each run is its own task: admission and the child fork of
                // one run must not serialize the admissions of the others
                // behind it inside this single tool task, or the burst never
                // reaches the 200-deep overlap the cap needs.
                running.spawn(start(index));
            }
            while running.join_next().await.is_some() {}
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("burst finished")))
        })
    }
}

struct Rig {
    agent: Agent,
    subscription: Subscription,
    workspace: PathBuf,
    turn: TurnId,
    results: RunResults,
    _host: Host,
    _tmp: tempfile::TempDir,
}

/// Builds the queue probe tool and its result sink.
fn build_probe(mode: Mode) -> Result<(Arc<QueueProbe>, RunResults), Box<dyn std::error::Error>> {
    let results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let probe = Arc::new(QueueProbe {
        name: Name::parse("fixture__queue")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__queue")?,
            description: "process queue probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        mode,
        results: Arc::clone(&results),
    });
    Ok((probe, results))
}

/// Starts one host with the probe registered and runs the one-step turn
/// that calls it. The probe holds the burst until the test releases it.
async fn start_rig(mode: Mode) -> Result<Rig, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let (probe, results) = build_probe(mode)?;
    let inject = dal_core::ServiceSet::from_names(["run"])?;
    let extension = ExtensionBuilder::new("fixture", "0.1.0", inject)?
        .with_origin(dal_core::Origin::Builtin, None)
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture")?,
                    kind: ExportKind::Tool,
                    local: Name::parse("queue")?,
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#)?,
                description: "process queue probe export".into(),
            },
        )
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (
                OsString::from("HOME"),
                OsString::from(tmp.path().join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.path().join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir.clone())?,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    let _ = agent
        .submit(Command::SetApproval {
            mode: dal_core::ApprovalMode::All,
            save: dal_core::Save::SessionOnly,
        })
        .await?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the queue probe".into(),
            }],
        })
        .await?;
    let dal_core::Reply::Accepted { turn, .. } = reply else {
        return Err(format!("prompt not accepted: {reply:?}").into());
    };
    Ok(Rig {
        agent,
        subscription,
        workspace: workspace_dir,
        turn,
        results,
        _host: host,
        _tmp: tmp,
    })
}

/// The newest progress row per call, as the subscription delivered them.
#[derive(Default)]
struct Rows {
    latest: HashMap<CallId, String>,
    /// Every progress row delivered, in arrival order.
    history: Vec<(CallId, String)>,
    ended: Option<bool>,
    after_end: Vec<CallId>,
}

impl Rows {
    fn observe(&mut self, delivery: &Delivery) {
        let Delivery::Update(update) = delivery else {
            return;
        };
        match &update.kind {
            UpdateKind::ToolProgress { call, tail } => {
                if self.ended.is_some() {
                    self.after_end.push(call.clone());
                }
                self.latest.insert(call.clone(), tail.to_string());
                self.history.push((call.clone(), tail.to_string()));
            }
            UpdateKind::TurnEnded { stop, .. } => {
                self.ended = Some(*stop == Stop::Cancelled);
            }
            _ => {}
        }
    }

    fn text(&self, call: &CallId) -> Option<&str> {
        self.latest.get(call).map(String::as_str)
    }

    /// Calls whose newest row says they wait for a process slot.
    fn waiting_calls(&self) -> Vec<CallId> {
        self.latest
            .iter()
            .filter(|(_, text)| text.starts_with(WAITING_PREFIX))
            .map(|(call, _)| call.clone())
            .collect()
    }

    fn dump(&self) -> String {
        let mut rows: Vec<_> = self
            .latest
            .iter()
            .map(|(call, text)| format!("{}={text:?}", call.as_str()))
            .collect();
        rows.sort();
        rows.truncate(12);
        format!("{} calls with rows, first {:?}", self.latest.len(), rows)
    }
}

/// Reads the subscription into `rows` until `check` says done. `check` may
/// also fail fast, which is how a wrongly started child is reported.
async fn settle(
    subscription: &mut Subscription,
    rows: &mut Rows,
    limit: Duration,
    what: &str,
    mut check: impl FnMut(&Rows) -> Result<bool, String>,
) -> Result<(), String> {
    let deadline = Instant::now() + limit;
    loop {
        if check(&*rows)? {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out waiting for {what}; {}", rows.dump()));
        }
        match tokio::time::timeout(remaining.min(POLL), subscription.next()).await {
            Ok(Some(delivery)) => rows.observe(&delivery),
            Ok(None) => return Err(format!("the subscription ended waiting for {what}")),
            Err(_) => {}
        }
    }
}

/// Reads whatever arrives for `span`, to bound an absence check.
async fn drain(subscription: &mut Subscription, rows: &mut Rows, span: Duration) {
    let deadline = Instant::now() + span;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, subscription.next()).await {
            Ok(Some(delivery)) => rows.observe(&delivery),
            Ok(None) | Err(_) => return,
        }
    }
}

/// Lets waiter `index` arrive and returns the call the host gave it.
///
/// The new waiter must show up as a waiting row with its own queue position,
/// counting itself, and must not have started a child.
async fn join_waiter(
    rig: &mut Rig,
    rows: &mut Rows,
    known: &HashMap<usize, CallId>,
    index: usize,
) -> Result<CallId, String> {
    let position = index - CAP + 1;
    let workspace = rig.workspace.clone();
    mark(&workspace, &format!("go-{index}"))
        .map_err(|error| format!("cannot create go-{index} to admit waiter {index}: {error}"))?;
    settle(
        &mut rig.subscription,
        rows,
        WAIT,
        &format!("waiter {index} to report queue position {position}"),
        |rows| {
            if has_pid(&workspace, index)? {
                return Err(format!(
                    "run {index} started a child past the {CAP}-process session cap instead of \
                     queueing with `{}`",
                    waiting(position)
                ));
            }
            Ok(rows.waiting_calls().len() > known.len())
        },
    )
    .await?;
    let call = rows
        .waiting_calls()
        .into_iter()
        .find(|call| !known.values().any(|seen| seen == call))
        .ok_or_else(|| format!("one new waiting call for waiter {index}"))?;
    assert_eq!(
        rows.text(&call),
        Some(waiting(position).as_str()),
        "waiter {index} reports the live queue length, itself included"
    );
    Ok(call)
}

/// Waits until `calls` all read `waiting(queued)`, failing if a call in
/// `never_started` started a child.
async fn every_waiter_shows(
    rig: &mut Rig,
    rows: &mut Rows,
    calls: &[CallId],
    never_started: &[usize],
    queued: usize,
) -> Result<(), String> {
    let workspace = rig.workspace.clone();
    let expected = waiting(queued);
    settle(
        &mut rig.subscription,
        rows,
        WAIT,
        &format!("every queued call to read `{expected}`"),
        |rows| {
            for index in never_started {
                if has_pid(&workspace, *index)? {
                    return Err(format!("run {index} started while it had to keep waiting"));
                }
            }
            Ok(calls
                .iter()
                .all(|call| rows.text(call) == Some(expected.as_str())))
        },
    )
    .await
}

/// Waits until the first 200 children all recorded their pids.
///
/// Shared by the gated, exec-door, and cancellation probes: the cap is only
/// provably full once every held child started. Unrelated pid read errors
/// fail the wait instead of looking like a child that never started.
async fn await_cap_full(rig: &mut Rig, rows: &mut Rows) -> Result<(), String> {
    let workspace = rig.workspace.clone();
    settle(
        &mut rig.subscription,
        rows,
        WAIT,
        "the first 200 children to start",
        |_| {
            for index in 0..CAP {
                if !has_pid(&workspace, index)? {
                    return Ok(false);
                }
            }
            Ok(true)
        },
    )
    .await
}

/// Waits until the turn publishes its end.
async fn await_turn_end(
    subscription: &mut Subscription,
    rows: &mut Rows,
    what: &str,
) -> Result<(), String> {
    settle(subscription, rows, WAIT, what, |rows| {
        Ok(rows.ended.is_some())
    })
    .await
}

/// Drains `span` after the turn ended and asserts nothing else is published.
///
/// Shared by the FIFO probe: the queue goes quiet once the turn ends.
async fn assert_quiet_after_end(subscription: &mut Subscription, rows: &mut Rows, span: Duration) {
    drain(subscription, rows, span).await;
    assert!(
        rows.after_end.is_empty(),
        "no progress is published after the turn ended: {:?}",
        rows.after_end
    );
}

/// Ends `step` to free its slot, then waits for `admitted` (the head waiter)
/// to spawn and to stop looking queued, failing if any waiter behind it or
/// the abandoned waiter starts first. Refreshes the live count over `behind`.
async fn admit_next(
    rig: &mut Rig,
    rows: &mut Rows,
    waiters: &HashMap<usize, CallId>,
    step: usize,
    admitted: usize,
    behind: &[usize],
) -> Result<(), String> {
    let workspace = rig.workspace.clone();
    end_child(&workspace, step)
        .map_err(|error| format!("cannot end child {step} to free its slot: {error}"))?;
    settle(
        &mut rig.subscription,
        rows,
        WAIT,
        &format!("waiter {admitted} to take the slot child {step} freed"),
        |_| {
            for index in behind {
                if has_pid(&workspace, *index)? {
                    return Err(format!(
                        "waiter {index} started before waiter {admitted}: the queue is not FIFO"
                    ));
                }
            }
            if has_pid(&workspace, ABANDONED)? {
                return Err(format!("the abandoned waiter {ABANDONED} spawned a child"));
            }
            has_pid(&workspace, admitted)
        },
    )
    .await?;
    let admitted_call = waiters[&admitted].clone();
    settle(
        &mut rig.subscription,
        rows,
        WAIT,
        &format!("waiter {admitted} to stop looking queued"),
        |rows| Ok(rows.text(&admitted_call) == Some("")),
    )
    .await?;
    if !behind.is_empty() {
        let behind_calls: Vec<CallId> = behind.iter().map(|index| waiters[index].clone()).collect();
        every_waiter_shows(rig, rows, &behind_calls, behind, behind.len()).await?;
    }
    Ok(())
}

/// The named acceptance workload: 210 concurrent `sleep 1` calls in one
/// session. Every real child holds its start on the fixture's `release`
/// marker, so the first 200 admitted slots stay held while all 210 run
/// attempts arrive and the 10 over the cap queue in view; after release the
/// measured work is the same one-second sleep for every child. At most 200
/// children are ever alive together, the 10 report exactly
/// `waiting for a process slot (10 queued)`, every call finishes once, and
/// no child outlives the turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_ten_sleep_one_runs_queue_ten_and_all_finish()
-> Result<(), Box<dyn std::error::Error>> {
    let _one = ONE_BURST_AT_A_TIME.lock().await;
    let mut rig = start_rig(Mode::Acceptance).await?;
    let workspace = rig.workspace.clone();
    let mut rows = Rows::default();

    // The owned start hold: the first 200 real children park on the
    // `release` marker with their slots held, so the 10 run attempts over
    // the cap queue behind a provably full cap no matter how loaded the
    // host is. A child past the cap starting here is a real red flag.
    settle(
        &mut rig.subscription,
        &mut rows,
        WAIT,
        "the 10 run attempts to queue behind the 200 held children",
        |rows| {
            let started_children =
                count_pids(&workspace).map_err(|error| format!("cannot count pids: {error}"))?;
            if started_children > CAP {
                return Err(format!(
                    "{started_children} children started past the {CAP}-process session cap"
                ));
            }
            let ten = waiting(LOAD - CAP);
            let at_ten: HashSet<&CallId> = rows
                .history
                .iter()
                .filter(|(_, text)| *text == ten)
                .map(|(call, _)| call)
                .collect();
            Ok(at_ten.len() == LOAD - CAP)
        },
    )
    .await
    .map_err(std::io::Error::other)?;
    // Release: every held child finishes its measured one-second sleep, and
    // each freed slot admits exactly one queued waiter whose child runs the
    // same one-second work.
    mark(&workspace, "release")?;

    await_turn_end(&mut rig.subscription, &mut rows, "the burst turn to end")
        .await
        .map_err(std::io::Error::other)?;
    assert_eq!(rows.ended, Some(false), "the burst turn ends normally");

    let ten = waiting(LOAD - CAP);
    let at_ten: HashSet<&CallId> = rows
        .history
        .iter()
        .filter(|(_, text)| *text == ten)
        .map(|(call, _)| call)
        .collect();
    assert_eq!(
        at_ten.len(),
        LOAD - CAP,
        "each of the 10 calls over the cap shows `{ten}`; rows seen: {}",
        rows.dump()
    );
    assert!(
        rows.history
            .iter()
            .filter(|(_, text)| text.starts_with(WAITING_PREFIX))
            .all(|(_, text)| (1..=LOAD - CAP).any(|queued| *text == waiting(queued))),
        "a waiting row carries a live count between 1 and 10"
    );
    assert!(
        rows.waiting_calls().is_empty(),
        "a call that started still looks queued; its sleeping child printed nothing: {}",
        rows.dump()
    );
    let peak = peak_active(&workspace)?;
    assert!(
        peak <= CAP,
        "{peak} children were alive together; the session cap is {CAP}"
    );

    let results = rig
        .results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut per_call = vec![0_usize; LOAD];
    for (index, text) in &results {
        per_call[*index] += 1;
        assert_eq!(text, "Exited(0)", "outcome of call {index}");
    }
    assert!(
        per_call.iter().all(|count| *count == 1),
        "every call settles exactly once: {per_call:?}"
    );

    let pids = std::fs::read_to_string(workspace.join("pids"))?;
    assert_eq!(pids.lines().count(), LOAD, "every call ran its child once");
    if Path::new("/proc/self").exists() {
        assert!(
            pids.lines()
                .filter_map(|line| line.parse::<u32>().ok())
                .all(|pid| !Path::new(&format!("/proc/{pid}")).exists()),
            "a child outlived the turn: the process count is not back to baseline"
        );
    }
    Ok(())
}

/// 200 held children plus 10 waiters: the queue is first in first out, the
/// count is live as waiters join, leave, and are admitted, a waiter that
/// leaves never spawns, and every call finishes once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn excess_runs_queue_fifo_with_a_live_count_and_each_finishes_once()
-> Result<(), Box<dyn std::error::Error>> {
    let _one = ONE_BURST_AT_A_TIME.lock().await;
    let mut rig = start_rig(Mode::Gated).await?;
    let workspace = rig.workspace.clone();
    let _reaper = Reaper {
        workspace: workspace.clone(),
    };
    let mut rows = Rows::default();

    await_cap_full(&mut rig, &mut rows)
        .await
        .map_err(std::io::Error::other)?;
    assert!(
        rows.waiting_calls().is_empty(),
        "nothing waits while the session is under its cap"
    );

    // Calls 201..=210 join one at a time, each reporting its own position.
    let mut waiters: HashMap<usize, CallId> = HashMap::new();
    for index in CAP..LOAD {
        let call = join_waiter(&mut rig, &mut rows, &waiters, index)
            .await
            .map_err(std::io::Error::other)?;
        waiters.insert(index, call);
    }
    let all: Vec<CallId> = waiters.values().cloned().collect();
    let beyond: Vec<usize> = (CAP..LOAD).collect();
    every_waiter_shows(&mut rig, &mut rows, &all, &beyond, LOAD - CAP)
        .await
        .map_err(std::io::Error::other)?;
    assert_eq!(
        started(&workspace)?,
        CAP,
        "at most 200 children are active; the other 10 wait"
    );

    // A waiter that leaves the queue shrinks every other waiter's count.
    mark(&workspace, &format!("abandon-{ABANDONED}"))?;
    let stay: Vec<usize> = (CAP..LOAD).filter(|index| *index != ABANDONED).collect();
    let stay_calls: Vec<CallId> = stay.iter().map(|index| waiters[index].clone()).collect();
    every_waiter_shows(&mut rig, &mut rows, &stay_calls, &beyond, stay.len())
        .await
        .map_err(std::io::Error::other)?;

    // Each freed slot starts exactly the head of the queue.
    for (step, admitted) in stay.iter().copied().enumerate() {
        let behind = &stay[step + 1..];
        admit_next(&mut rig, &mut rows, &waiters, step, admitted, behind)
            .await
            .map_err(std::io::Error::other)?;
    }
    assert_eq!(
        started(&workspace)?,
        CAP + stay.len(),
        "every waiter that stayed ran, and the abandoned one never did"
    );

    // Let every remaining child go; the burst, then the turn, finishes.
    for index in stay.len()..LOAD {
        if index != ABANDONED && has_pid(&workspace, index).map_err(std::io::Error::other)? {
            end_child(&workspace, index)?;
        }
    }
    await_turn_end(&mut rig.subscription, &mut rows, "the burst turn to end")
        .await
        .map_err(std::io::Error::other)?;
    assert_eq!(rows.ended, Some(false), "the burst turn ends normally");

    let results = rig
        .results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut per_call = vec![0_usize; LOAD];
    for (index, _) in &results {
        per_call[*index] += 1;
    }
    assert!(
        per_call.iter().all(|count| *count == 1),
        "every call settles exactly once: {per_call:?}"
    );
    for (index, text) in &results {
        let expected = if *index == ABANDONED {
            "abandoned"
        } else {
            "Signaled(15)"
        };
        assert_eq!(text, expected, "outcome of call {index}");
    }
    assert!(
        !has_pid(&workspace, ABANDONED).map_err(std::io::Error::other)?,
        "the abandoned waiter never spawned"
    );
    assert_quiet_after_end(&mut rig.subscription, &mut rows, Duration::from_millis(700)).await;
    Ok(())
}

/// The exec door: the 201st `authorize` of a session with 200 live children
/// waits instead of spawning and reports `(1 queued)` under its call, a freed
/// slot admits it, the next one waits and reports again, and a cancelled turn
/// never lets the waiting authorization spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_exec_door_queues_the_next_authorization_and_never_spawns_after_cancel()
-> Result<(), Box<dyn std::error::Error>> {
    let _one = ONE_BURST_AT_A_TIME.lock().await;
    let mut rig = start_rig(Mode::Exec).await?;
    let workspace = rig.workspace.clone();
    let _reaper = Reaper {
        workspace: workspace.clone(),
    };
    let mut rows = Rows::default();

    await_cap_full(&mut rig, &mut rows)
        .await
        .map_err(std::io::Error::other)?;

    settle(
        &mut rig.subscription,
        &mut rows,
        WAIT,
        "the 201st authorization to report that it waits",
        |rows| {
            if has_pid(&workspace, CAP)? {
                return Err(format!(
                    "child {CAP} started past the {CAP}-process session cap instead of queueing \
                     with `{}`",
                    waiting(1)
                ));
            }
            Ok(!rows.waiting_calls().is_empty())
        },
    )
    .await
    .map_err(std::io::Error::other)?;
    let call = rows
        .waiting_calls()
        .into_iter()
        .next()
        .ok_or_else(|| std::io::Error::other("the waiting call"))?;
    assert_eq!(rows.text(&call), Some(waiting(1).as_str()));

    // One child ends; the waiting authorization takes its slot and spawns.
    end_child(&workspace, 0)?;
    settle(
        &mut rig.subscription,
        &mut rows,
        WAIT,
        "the waiting authorization to take the freed slot",
        |_| has_pid(&workspace, CAP),
    )
    .await
    .map_err(std::io::Error::other)?;

    // The call's next authorization waits again and publishes a fresh row.
    let again = |rows: &Rows| {
        rows.history
            .iter()
            .filter(|(key, text)| *key == call && *text == waiting(1))
            .count()
    };
    settle(
        &mut rig.subscription,
        &mut rows,
        WAIT,
        "the next authorization to publish its own waiting row",
        |rows| {
            if has_pid(&workspace, CAP + 1)? {
                return Err(format!("child {} started past the session cap", CAP + 1));
            }
            Ok(again(rows) >= 2)
        },
    )
    .await
    .map_err(std::io::Error::other)?;

    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(rig.turn),
        })
        .await?;
    await_turn_end(
        &mut rig.subscription,
        &mut rows,
        "the cancelled turn to end",
    )
    .await
    .map_err(std::io::Error::other)?;
    assert_eq!(rows.ended, Some(true), "the turn ends cancelled");
    drain(
        &mut rig.subscription,
        &mut rows,
        Duration::from_millis(1500),
    )
    .await;
    assert!(
        !has_pid(&workspace, CAP + 1).map_err(std::io::Error::other)?,
        "a cancelled authorization spawned a child"
    );
    assert!(
        rows.after_end.is_empty(),
        "progress was published after the cancelled turn ended: {:?}",
        rows.after_end
    );
    Ok(())
}

/// Cancelling the turn while calls wait for a slot: no waiter ever spawns,
/// even though the cancelled children free their slots, and nothing is
/// published for a waiter after the turn ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_the_turn_never_spawns_a_queued_run() -> Result<(), Box<dyn std::error::Error>> {
    let _one = ONE_BURST_AT_A_TIME.lock().await;
    let mut rig = start_rig(Mode::Gated).await?;
    let workspace = rig.workspace.clone();
    let _reaper = Reaper {
        workspace: workspace.clone(),
    };
    let mut rows = Rows::default();

    await_cap_full(&mut rig, &mut rows)
        .await
        .map_err(std::io::Error::other)?;

    let mut waiters: HashMap<usize, CallId> = HashMap::new();
    for index in CAP..CAP + 3 {
        let call = join_waiter(&mut rig, &mut rows, &waiters, index)
            .await
            .map_err(std::io::Error::other)?;
        waiters.insert(index, call);
    }
    let calls: Vec<CallId> = waiters.values().cloned().collect();
    let queued: Vec<usize> = (CAP..CAP + 3).collect();
    every_waiter_shows(&mut rig, &mut rows, &calls, &queued, 3)
        .await
        .map_err(std::io::Error::other)?;

    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(rig.turn),
        })
        .await?;
    await_turn_end(
        &mut rig.subscription,
        &mut rows,
        "the cancelled turn to end",
    )
    .await
    .map_err(std::io::Error::other)?;
    assert_eq!(rows.ended, Some(true), "the turn ends cancelled");

    // The cancelled children free 200 slots; no queued run may take one.
    drain(
        &mut rig.subscription,
        &mut rows,
        Duration::from_millis(1500),
    )
    .await;
    for index in &queued {
        assert!(
            !has_pid(&workspace, *index).map_err(std::io::Error::other)?,
            "a cancelled waiter spawned a child"
        );
    }
    assert!(
        rows.after_end.is_empty(),
        "progress was published after the cancelled turn ended: {:?}",
        rows.after_end
    );
    Ok(())
}
