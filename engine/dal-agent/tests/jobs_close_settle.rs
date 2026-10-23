#![expect(
    clippy::expect_used,
    reason = "fixture setup fails loudly at its boundary"
)]
//! Cancelled detached jobs settle and session close kills every process group.
//!
//! R2/R3 regression (report-t-stress-decoder residuals): after `agents cancel`
//! closes a child session, its parked detached exec jobs must each publish a
//! terminal ledger end, and no spawned process group may survive the 3 s
//! contract window. The session close path used to drop `Proc` handles instead
//! of routing detached jobs through `Proc::stop`: `SessionTasks::stop`
//! cancelled the reap tasks, so `settle_once` never ran (no ledger end, the
//! run never completed), and `Drop for Proc` killed only the leader, leaving
//! process-group siblings alive past the census.
//!
//! Failing-before signature on the old code: no `reports.jsonl` end line for
//! the detached job after `host.close`, and the sibling `sleep` PIDs/groups
//! still alive 3 s after close.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Env, Host, ProcStatus, Product, SessionRef, SpawnOpts};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Expect, ModelId, ModelInfo,
    ModelRequest, Name, Part, RawJson, ServiceSet, ThinkingLevel, ToolClass, ToolSpec, Usage,
    Visibility, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs, ToolCall as ProviderCall};

/// Bound for every settle wait; failures assert on the recorded state.
const WAIT: Duration = Duration::from_secs(60);
/// Poll step for record flags and files.
const POLL: Duration = Duration::from_millis(50);
/// How fast every recorded tree must vanish after close, mirroring the gate.
const GONE_WITHIN: Duration = Duration::from_secs(3);
/// The long sleep no test phase ever reaches; close must kill it early.
const SLEEP_S: u64 = 120;
/// Foreground budget of the detached job: it parks after one second.
const DETACH_BUDGET_S: u64 = 1;

const PARK_MODEL: &str = "close-settle/park";

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        output_tokens: 1,
        cached_input_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    }
}

fn stream_of(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
}

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

fn call_stream(id: &str, tool: &str) -> EventStream {
    stream_of(vec![
        Ok(StreamEvent::ToolCallStarted {
            id: id.to_owned(),
            name: tool.to_owned(),
        }),
        Ok(StreamEvent::ToolCallsDone {
            calls: vec![ProviderCall {
                id: id.to_owned(),
                name: tool.to_owned(),
                args: ToolArgs::Parsed(RawJson::parse("{}").expect("empty args")),
            }],
        }),
        Ok(StreamEvent::Usage { usage: usage() }),
        Ok(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        }),
    ])
}

fn tool_results(context: &[ContextItem], tool: &str) -> usize {
    context
        .iter()
        .filter(|item| matches!(item, ContextItem::ToolResult { name, .. } if &**name == tool))
        .count()
}

/// The scripted model: first `park` (detaches after one second), then `hold`
/// (parks the turn in the foreground wait), then ends the turn. With
/// `hold_only` it skips the detach, isolating a raw close with no reaper.
struct ParkModel {
    hold_only: bool,
}

impl ModelHandler for ParkModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let context: &[ContextItem] = request.context.as_ref();
            if !self.hold_only && tool_results(context, "park") == 0 {
                return Ok(call_stream("c-park", "park"));
            }
            if tool_results(context, "hold") == 0 {
                return Ok(call_stream("c-hold", "hold"));
            }
            Ok(text_stream("done"))
        })
    }
}

fn caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

#[derive(Default)]
struct CloseRecords {
    detached_job: Option<String>,
    hold_entered: bool,
}

/// Spawns `command` through the checked process door.
fn spawn_argv(command: &str) -> Vec<OsString> {
    vec![
        OsString::from("/bin/sh"),
        OsString::from("-c"),
        OsString::from(command),
    ]
}

async fn authorize_and_spawn(
    cx: &mut ToolCx<'_>,
    pid_file: &str,
) -> Result<dal_agent::Proc, dal_agent::ToolError> {
    let preview = dal_core::Preview {
        title: "close-settle".into(),
        body: pid_file.into(),
        digest: None,
    };
    let approved = cx
        .authorize(preview)
        .await
        .map_err(dal_agent::ToolError::Denied)?;
    let command = format!("true; sleep {SLEEP_S} & echo \"$$ $!\" > {pid_file}; sleep {SLEEP_S}");
    let opts = SpawnOpts {
        cwd: cx.workspace().as_path().to_path_buf(),
        timeout: None,
        env: Vec::new(),
        stdout_prefix_limit: 0,
    };
    cx.spawn(&spawn_argv(&command), opts, approved)
}

/// Detaches after a one-second foreground budget, mirroring the gate's
/// background exec shape: the reaper owns the tree from here on.
struct ParkTool {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<CloseRecords>>,
}

impl Tool for ParkTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let mut proc = match authorize_and_spawn(&mut cx, "close-settle-detached.pids").await {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            let cancel = cx.cancel().clone();
            let job = tokio::select! {
                biased;
                result = proc.wait(&cancel) => {
                    let text = match result {
                        Ok(result) => format!("exited early: {:?}", result.status),
                        Err(error) => format!("wait failed: {error}"),
                    };
                    return ToolOutcome::Err(dal_agent::ToolError::message(text));
                }
                () = tokio::time::sleep(Duration::from_secs(DETACH_BUDGET_S)) => cx.detach(proc),
            };
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .detached_job = Some(job.to_string());
            ToolOutcome::Detached(job)
        })
    }
}

/// Parks the turn in the foreground wait: session close aborts this future,
/// so the group must die through the close path, not through a settle.
struct HoldTool {
    name: Name,
    spec: Arc<ToolSpec>,
    records: Arc<Mutex<CloseRecords>>,
}

impl Tool for HoldTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let mut proc = match authorize_and_spawn(&mut cx, "close-settle-hold.pids").await {
                Ok(proc) => proc,
                Err(error) => return ToolOutcome::Err(error),
            };
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hold_entered = true;
            match proc.wait(cx.cancel()).await {
                Ok(result) => match result.status {
                    ProcStatus::Cancelled => ToolOutcome::Interrupted,
                    status => ToolOutcome::Err(dal_agent::ToolError::message(format!(
                        "ended: {status:?}"
                    ))),
                },
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

fn tool_spec(name: &Name) -> Arc<ToolSpec> {
    Arc::new(ToolSpec {
        name: name.clone(),
        description: "close-settle fixture tool".into(),
        parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        grammar: None,
    })
}

struct Rig {
    _tmp: tempfile::TempDir,
    host: Host,
    data: PathBuf,
    workspace: PathBuf,
    records: Arc<Mutex<CloseRecords>>,
}

async fn rig() -> Rig {
    rig_with(false).await
}

async fn rig_hold_only() -> Rig {
    rig_with(true).await
}

async fn rig_with(hold_only: bool) -> Rig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, "").expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"{PARK_MODEL}\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\"),
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let records = Arc::new(Mutex::new(CloseRecords::default()));
    let park_name = Name::parse("park").expect("tool name");
    let hold_name = Name::parse("hold").expect("tool name");
    let extension = ExtensionBuilder::new("close-settle", "0.1.0", ServiceSet::default())
        .expect("builder")
        .tool(
            Arc::new(ParkTool {
                name: park_name.clone(),
                spec: tool_spec(&park_name),
                records: Arc::clone(&records),
            }),
            Visibility::Model,
        )
        .tool(
            Arc::new(HoldTool {
                name: hold_name.clone(),
                spec: tool_spec(&hold_name),
                records: Arc::clone(&records),
            }),
            Visibility::Model,
        )
        .model(ModelRecord {
            id: ModelId::parse(PARK_MODEL).expect("model id"),
            caps: caps(),
            handler: Arc::new(ParkModel { hold_only }),
            export: None,
        })
        .build()
        .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    Rig {
        _tmp: tmp,
        host,
        data,
        workspace: workspace_dir,
        records,
    }
}

/// Reads `/proc/<pid>/stat`: `None` when reaped, `Some(state)` otherwise.
fn proc_state(pid: i32) -> Option<char> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = text.rsplit(')').next()?;
    after.split_whitespace().next()?.chars().next()
}

/// A zombie is already dead: it answers signals but holds no tree.
fn pid_alive(pid: i32) -> bool {
    proc_state(pid).is_some_and(|state| state != 'Z')
}

/// Every non-zombie member's process group, mirroring the gate census.
fn live_groups() -> std::collections::BTreeSet<i32> {
    let mut live = std::collections::BTreeSet::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return live;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.bytes().any(|byte| !byte.is_ascii_digit()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        let Some(after) = text.rsplit(')').next() else {
            continue;
        };
        let fields: Vec<&str> = after.split_whitespace().collect();
        if fields.first().is_some_and(|state| state.starts_with('Z')) {
            continue;
        }
        if let Some(pgrp) = fields.get(2).and_then(|text| text.parse().ok()) {
            live.insert(pgrp);
        }
    }
    live
}

fn read_pids(path: &Path) -> Option<Vec<i32>> {
    let text = std::fs::read_to_string(path).ok()?;
    let pids: Vec<i32> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<i32>, _>>()
        .ok()?;
    (!pids.is_empty()).then_some(pids)
}

fn find_reports(data: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![data.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "reports.jsonl") {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test]
async fn hold_only_close_completes_and_kills_the_parked_group() {
    let rig = rig_hold_only().await;
    let agent = rig
        .host
        .open(
            SessionRef::New {
                workspace: Workspace::new(rig.workspace.clone()).expect("workspace"),
                name: None,
            },
            ClientId::new("close-settle-test"),
        )
        .await
        .expect("session");
    let session = agent.session();

    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hold foreground".into(),
            }],
        })
        .await
        .expect("prompt accepted");

    // The hold call parks the turn in the foreground wait; no detached job
    // exists, so no reaper is involved at all.
    let deadline = Instant::now() + WAIT;
    let hold_pids = loop {
        let hold_entered = rig
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hold_entered;
        let hold_pids = read_pids(&rig.workspace.join("close-settle-hold.pids"));
        if let Some(hold) = hold_pids
            && hold_entered
        {
            break hold;
        }
        assert!(Instant::now() < deadline, "the hold tree parked");
        tokio::time::sleep(POLL).await;
    };
    assert!(
        hold_pids.len() >= 2,
        "the hold tree reports its shell and sibling: {hold_pids:?}"
    );
    eprintln!("close-settle: hold parked, closing without any reaper");

    let closed = tokio::time::timeout(Duration::from_secs(60), rig.host.close(session)).await;
    let closed = closed.expect("raw close returns instead of hanging the suite");
    closed.expect("the session closes");
    eprintln!("close-settle: hold-only session closed");

    let groups: Vec<i32> = vec![hold_pids[0]];
    let start = Instant::now();
    loop {
        let live_pids: Vec<i32> = hold_pids
            .iter()
            .copied()
            .filter(|pid| pid_alive(*pid))
            .collect();
        let groups_now = live_groups();
        let live_groups: Vec<i32> = groups
            .iter()
            .copied()
            .filter(|group| groups_now.contains(group))
            .collect();
        if live_pids.is_empty() && live_groups.is_empty() {
            break;
        }
        assert!(
            start.elapsed() <= GONE_WITHIN,
            "{} PIDs {live_pids:?} and {} groups {live_groups:?} still alive after close",
            live_pids.len(),
            live_groups.len(),
        );
        tokio::time::sleep(POLL).await;
    }
    eprintln!("close-settle: hold-only group gone");

    let shutdown = rig.host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "no session task outlives the close"
    );
}

/// Waits until both trees parked: the detached job id plus both pid files,
/// with the hold entered. The detached job parks after its one-second
/// budget; the hold call parks the turn in the foreground.
async fn wait_both_trees_parked(rig: &Rig) -> (String, Vec<i32>, Vec<i32>) {
    let deadline = Instant::now() + WAIT;
    loop {
        let detached_job = rig
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detached_job
            .clone();
        let hold_entered = rig
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hold_entered;
        let detached_pids = read_pids(&rig.workspace.join("close-settle-detached.pids"));
        let hold_pids = read_pids(&rig.workspace.join("close-settle-hold.pids"));
        if let (Some(job), Some(detached), Some(hold)) = (detached_job, detached_pids, hold_pids)
            && hold_entered
        {
            break (job, detached, hold);
        }
        assert!(
            Instant::now() < deadline,
            "both trees parked before the bound"
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn cancelled_detached_jobs_settle_and_close_kills_every_group() {
    let rig = rig().await;
    let agent = rig
        .host
        .open(
            SessionRef::New {
                workspace: Workspace::new(rig.workspace.clone()).expect("workspace"),
                name: None,
            },
            ClientId::new("close-settle-test"),
        )
        .await
        .expect("session");
    let session = agent.session();

    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "park both".into(),
            }],
        })
        .await
        .expect("prompt accepted");

    // Both trees report their PIDs: the detached job parks after its
    // one-second budget, the hold call parks the turn in the foreground.
    let (detached_job, detached_pids, hold_pids) = wait_both_trees_parked(&rig).await;
    assert!(
        detached_pids.len() >= 2 && hold_pids.len() >= 2,
        "each tree reports its shell and sibling: {detached_pids:?} {hold_pids:?}"
    );
    eprintln!("close-settle: both trees parked (detached job {detached_job}), closing");

    let closed = tokio::time::timeout(Duration::from_secs(60), rig.host.close(session)).await;
    let closed = closed.expect("raw close returns instead of hanging the suite");
    closed.expect("the session closes");
    eprintln!("close-settle: session closed");

    // R2: the cancelled detached job publishes its terminal ledger end.
    let deadline = Instant::now() + Duration::from_secs(15);
    let ended = loop {
        let mut ended = false;
        for reports in find_reports(&rig.data) {
            let text = std::fs::read_to_string(&reports).unwrap_or_default();
            if text.lines().any(|line| {
                line.contains(detached_job.as_str()) && line.contains("\"state\":\"cancelled\"")
            }) {
                ended = true;
                break;
            }
        }
        if ended || Instant::now() >= deadline {
            break ended;
        }
        tokio::time::sleep(POLL).await;
    };
    assert!(
        ended,
        "the cancelled detached job {detached_job} settles with a cancelled ledger end"
    );
    eprintln!("close-settle: detached ledger end observed");

    // R3: no recorded PID and no recorded group survives the contract window.
    let groups: Vec<i32> = vec![detached_pids[0], hold_pids[0]];
    let pids: Vec<i32> = detached_pids
        .iter()
        .chain(hold_pids.iter())
        .copied()
        .collect();
    let start = Instant::now();
    loop {
        let live_pids: Vec<i32> = pids.iter().copied().filter(|pid| pid_alive(*pid)).collect();
        let groups_now = live_groups();
        let live_groups: Vec<i32> = groups
            .iter()
            .copied()
            .filter(|group| groups_now.contains(group))
            .collect();
        if live_pids.is_empty() && live_groups.is_empty() {
            break;
        }
        assert!(
            start.elapsed() <= GONE_WITHIN,
            "{} PIDs {live_pids:?} and {} groups {live_groups:?} still alive after close",
            live_pids.len(),
            live_groups.len(),
        );
        tokio::time::sleep(POLL).await;
    }

    let shutdown = rig.host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "no session task outlives the close"
    );
}
