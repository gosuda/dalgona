// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion scale gate (`five-hundred-subagents-scale`): one
//! `agents` run of 500 subagents over the real host. 250 of them are shared
//! readers and 250 are writers in their own git worktrees, each creating a
//! different file. Every subagent reports `done`, the run reaches the model
//! once, every writer's file lands in the real checkout, no worktree or
//! retained tree is left behind, and the process holds its resource contract
//! (RSS under 256 MiB, open fds under 2048) at its peak, not only after the
//! run.
//!
//! Set `DALGONA_ORCH_STRESS=1` to run; the default suite skips the minutes
//! long fan-out, matching the contract's stress-gate opt-in.
//!
//! The model boundary is deterministic without touching the shared replay
//! fixture: a registered synthetic model (the public `ModelHandler`
//! extension API, as `wires-08` uses it) answers each request from its own
//! context. A reader child reports `done` through `orchestration__report`. A
//! writer child first creates the file named by its pool item through the
//! `patch` tool, in the `replace` edit style the gate selects, then reports.
//! A request that has already seen an `agents` tool result gets the closing
//! text. The shared FIFO replay fixture cannot give that, because one queue
//! serves every session in script order.
//!
//! The two steps run one after the other (`after`), so the 64-worker pool
//! limit of each step is also the limit of the whole run.
//!
//! Not reachable from this target, and not claimed here: the merge-lock probe
//! (merges never overlap) and the battery mailbox depth (never above 256) are
//! private to `dalgona-batteries`, and the TUI keypress echo P99 needs the
//! PTY harness of the terminal gates. The observable consequence of the lock,
//! that 250 concurrent merges against one checkout all apply cleanly, is
//! asserted.
#![expect(
    clippy::disallowed_methods,
    reason = "gate reads its opt-in flag and scratch dirs from the environment"
)]

use dalgona_gates::support;

use futures::FutureExt as _;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_core::{
    Caps, ContextItem, ModelId, ModelRequest, ModelToolSpec, Part, RawJson, ServiceSet,
    ThinkingLevel, Usage,
};
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs, ToolCall};

const SHARED_ITEMS: usize = 250;
const WRITER_ITEMS: usize = 250;
const ITEMS: usize = SHARED_ITEMS + WRITER_ITEMS;
const WORKERS: usize = 64;
const RSS_LIMIT_MIB: u64 = 256;
const FD_LIMIT: usize = 2_048;
const DELIVERY_WAIT: Duration = Duration::from_secs(600);
/// How often the peak sampler reads the process counters: every second,
/// except on Windows where each reading spawns a PowerShell process.
#[cfg(windows)]
const SAMPLE_PERIOD: Duration = Duration::from_secs(5);
#[cfg(not(windows))]
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);
const GATE_CLIENT: &str = "stress-gate";

const POOL_MODEL: &str = "stress-gate/pool";
const ROOT_MODEL: &str = "stress-gate/root";
const REPORT_TOOL_NAME: &str = "orchestration__report";
const PATCH_TOOL_NAME: &str = "patch";
/// The pool item text of a writer; the digits that follow name its file.
const WRITER_TAG: &str = "writer-unit-";

/// The model-facing `agents` call gate 09 submits: a reader pool of 250 items
/// and, after it, a writer pool of 250 items in worktrees, both over 64
/// workers.
fn pool_call() -> String {
    let readers = (0..SHARED_ITEMS)
        .map(|index| format!(r#""item {index}""#))
        .collect::<Vec<_>>()
        .join(",");
    let writers = (0..WRITER_ITEMS)
        .map(|unit| format!(r#""{WRITER_TAG}{unit:04}""#))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"action":"run","name":"pool","steps":[{{"name":"map","items":[{readers}],"workers":{WORKERS},"prompt":"process {{{{item}}}} and report"}},{{"name":"write","after":["map"],"items":[{writers}],"workers":{WORKERS},"tools":["patch"],"isolation":"worktree","prompt":"create the file for {{{{item}}}} then report"}}]}}"#
    )
}

const REPORT_CALL: &str = r#"{"status":"done","report":"processed"}"#;

/// The file a writer creates, relative to its worktree root.
fn writer_file(unit: usize) -> String {
    format!("unit-{unit:04}.txt")
}

/// The `patch` call of the `replace` edit style that creates one writer file.
fn patch_call(unit: usize) -> String {
    format!(
        r#"{{"changes":[{{"path":"{}","create":"unit {unit:04}\n"}}]}}"#,
        writer_file(unit)
    )
}

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The one synthetic model the gate registers: every request is answered
/// from the request's own context, so no shared step queue exists.
struct PoolModel {
    /// Completed `agents` runs observed in the context; the closing text is
    /// issued once per completed run.
    runs: AtomicUsize,
}

impl ModelHandler for PoolModel {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let answered = |name: &str| context_saw_tool(request.context.as_ref(), name);
            // A child step sees the report tool, and a writer step also the
            // patch tool. A writer creates its file first; every child then
            // reports done, and the request after the report result ends the
            // child's turn.
            if child_reports(request.tools.as_ref()) {
                if child_patches(request.tools.as_ref()) && !answered(PATCH_TOOL_NAME) {
                    let unit =
                        writer_unit(request.context.as_ref()).ok_or(ModelError::SecondForward)?;
                    return tool_call_stream(
                        &format!("p{unit}"),
                        PATCH_TOOL_NAME,
                        &patch_call(unit),
                    )
                    .map_err(|_| ModelError::SecondForward);
                }
                if answered(REPORT_TOOL_NAME) {
                    return Ok(text_stream("reported"));
                }
                let id = format!("c{}", request.cache_key.as_deref().unwrap_or(""));
                return tool_call_stream(&id, REPORT_TOOL_NAME, REPORT_CALL)
                    .map_err(|_| ModelError::SecondForward);
            }
            // The delivery wake re-runs the parent turn after a completed
            // run; its context carries the `agents` tool result.
            if answered("agents") {
                let served = self.runs.fetch_add(1, Ordering::AcqRel);
                return Ok(text_stream(&format!("pool run {served} complete")));
            }
            // The root's first request starts the pool.
            tool_call_stream("r1", "agents", &pool_call()).map_err(|_| ModelError::SecondForward)
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

/// The writer unit number the child's prompt names: the digits after the
/// writer tag in the first user message that carries it.
fn writer_unit(context: &[ContextItem]) -> Option<usize> {
    context.iter().find_map(|item| {
        let ContextItem::User { parts } = item else {
            return None;
        };
        parts.iter().find_map(|part| {
            let Part::Text { text } = part else {
                return None;
            };
            let text: &str = text;
            let rest = &text[text.find(WRITER_TAG)? + WRITER_TAG.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    })
}

/// Whether the visible tool surface contains the pool step's report tool:
/// only such a child request can report.
fn child_reports(tools: &[ModelToolSpec]) -> bool {
    tools
        .iter()
        .any(|tool| tool.name.as_ref() == REPORT_TOOL_NAME)
}

/// Whether the visible tool surface contains `patch`: only a writer step's
/// child can edit.
fn child_patches(tools: &[ModelToolSpec]) -> bool {
    tools
        .iter()
        .any(|tool| tool.name.as_ref() == PATCH_TOOL_NAME)
}

/// Wraps one response's events into an `EventStream`.
fn stream_of(events: Vec<Result<StreamEvent, ProviderError>>) -> EventStream {
    EventStream::new(futures::stream::iter(events), || {})
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
    }
}

/// The synthetic-model extension the gate adds to the assembled product.
fn pool_extension() -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let root = pool_record(POOL_MODEL)?;
    let wake = pool_record(ROOT_MODEL)?;
    ExtensionBuilder::new("stress-pool", "0.1.0", ServiceSet::default())?
        .model(root)
        .model(wake)
        .build()
}

fn pool_record(id: &str) -> Result<ModelRecord, dal_core::RegistrationError> {
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: Caps {
            context_window: Some(200_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(PoolModel {
            runs: AtomicUsize::new(0),
        }),
        export: None,
    })
}

#[test]
fn stress_500_pool_items_finish_within_the_process_contract() -> support::TestResult<()> {
    if std::env::var_os("DALGONA_ORCH_STRESS").is_none() {
        // The default suite skips the minutes-long fan-out, matching the
        // contract's stress-gate opt-in.
        return Ok(());
    }
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-stress-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    // The writers' worktrees branch from a real commit of a real checkout.
    support::init_git_checkout(&workspace)?;
    // `replace` takes the JSON `changes` envelope; the product default,
    // `hashline`, takes a freeform payload instead.
    let user = format!(
        "model = \"{ROOT_MODEL}\"\napproval = \"all\"\nedit_style = \"replace\"\n\n[judge]\ngate = \"off\"\n"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_scenario(&data, &workspace, &user))?;
    // The host is shut down and every task of the run has settled, so the
    // checkout is read at rest.
    verify_writers_merged(&workspace, &data)
}

/// Prints the completion marker of one scenario: the workflow greps for it.
fn scenario_ok(marker: &str) {
    eprintln!("{}", support::scenario_marker(marker));
}

/// One pump of a subscription: a ready delivery, an orderly end, or nothing
/// ready yet. `Ended` is distinct from `Empty`: only the former fails the
/// slow-reader verdict.
enum Pump {
    Delivery(dal_agent::Delivery),
    Empty,
    Ended,
}

fn pump(subscription: &mut dal_agent::Subscription) -> Pump {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let future = subscription.next();
    tokio::pin!(future);
    match future.poll(&mut context) {
        Poll::Ready(Some(delivery)) => Pump::Delivery(delivery),
        Poll::Ready(None) => Pump::Ended,
        Poll::Pending => Pump::Empty,
    }
}

/// Spawns the 250 ms slow reader on `agent`: every drain goes to a shared
/// [`support::SubscriberLedger`], which fails on any `Resync` or early end.
fn spawn_slow_reader(
    agent: &dal_agent::Agent,
    stop: &Arc<AtomicBool>,
) -> support::TestResult<tokio::task::JoinHandle<support::SubscriberLedger>> {
    let mut subscription = agent.subscribe_listen(None)?;
    let stop = Arc::clone(stop);
    Ok(tokio::spawn(async move {
        let mut ledger = support::SubscriberLedger::default();
        loop {
            tokio::time::sleep(support::SUBSCRIBER_PERIOD).await;
            let mut count = 0;
            let mut resync = 0;
            let mut jobs: Vec<String> = Vec::new();
            let mut relevant = 0;
            let mut ended = false;
            loop {
                match pump(&mut subscription) {
                    Pump::Delivery(dal_agent::Delivery::Update(update)) => {
                        count += 1;
                        if let dal_core::UpdateKind::JobStarted { job }
                        | dal_core::UpdateKind::JobSettled { job } = &update.kind
                        {
                            jobs.push(job.to_string());
                        }
                        // Scale/v9 production emits no Job updates; the load
                        // proof here is settled tool work and ended turns.
                        if let dal_core::UpdateKind::ToolSettled { .. }
                        | dal_core::UpdateKind::TurnEnded { .. } = &update.kind
                        {
                            relevant += 1;
                        }
                    }
                    Pump::Delivery(dal_agent::Delivery::Resync { .. }) => {
                        count += 1;
                        resync += 1;
                    }
                    Pump::Empty => break,
                    Pump::Ended => {
                        ended = true;
                        break;
                    }
                }
            }
            if ended {
                ledger.record_early_end();
                break;
            }
            if count > 0 || stop.load(Ordering::Acquire) {
                ledger.record_drain(count, resync, &jobs, relevant, Instant::now());
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
        }
        ledger
    }))
}

/// Runs the pool through one real host and asserts the delivery, the wake, and
/// the process contract at its peak.
async fn run_scenario(data: &Path, workspace: &Path, user: &str) -> support::TestResult<()> {
    let started = Instant::now();
    support::grant_bundled_extensions(data, workspace, user, GATE_CLIENT).await?;
    let mut product = support::build_product(data.to_path_buf(), Some(user))?;
    product.extensions.push(pool_extension()?);
    let env = dal_agent::Env {
        vars: std::collections::BTreeMap::default(),
        cwd: workspace.to_path_buf(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, support::host_config(data, user)?, env).await?;
    let agent = host
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(workspace.to_path_buf())?,
                name: None,
            },
            dal_core::ClientId::new(GATE_CLIENT),
        )
        .await?;
    let session = agent
        .view(dal_core::PageReq::default())?
        .session
        .id
        .to_string();
    let mut lifecycle = host.subscribe();
    let stop = Arc::new(AtomicBool::new(false));
    let slow = spawn_slow_reader(&agent, &stop)?;
    let sampler = support::PeakSampler::start(SAMPLE_PERIOD);
    let rss_before_prompt = rss_mib().await?;
    agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![Part::Text {
                text: "run the pool".into(),
            }],
        })
        .await?;
    let journal = wait_for_wake_turn_end(data, &session).await?;
    let elapsed = started.elapsed();
    assert_one_delivery(&journal);

    let rss_post_wake = rss_mib().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let rss_post_settle = rss_mib().await?;
    let rss_settled = rss_post_settle.max(rss_post_wake);
    let (opened, removed) = drained_lifecycle(&mut lifecycle);
    eprintln!(
        "stress resources: before={rss_before_prompt} MiB, wake={rss_post_wake} MiB, \
         settled={rss_post_settle} MiB, children opened={opened}, removed={removed}, \
         readers={SHARED_ITEMS}, worktree writers={WRITER_ITEMS}, run={elapsed:?}"
    );
    assert!(
        rss_settled < RSS_LIMIT_MIB,
        "RSS {rss_settled} MiB exceeds the {RSS_LIMIT_MIB} MiB cap \
         (before-prompt {rss_before_prompt}, post-wake {rss_post_wake}, post-settle {rss_post_settle})"
    );
    let fds = count_fds().await?;
    assert!(fds <= FD_LIMIT, "open fds {fds} exceed the {FD_LIMIT} cap");
    // The reader stops before shutdown: after it the subscription ends, and
    // an end past the stop flag would fail the verdict spuriously.
    stop.store(true, Ordering::Release);
    let ledger = slow
        .await
        .map_err(|join| format!("slow reader failed: {join}"))?;
    eprintln!("scale-500 subscriber: {}", ledger.summary());
    // Production emits no Job updates, so the per-job rate holds vacuously
    // here; the proven bound is no-Resync under the 500-child delivery load.
    ledger.verdict()?;
    let report = host.shutdown(Duration::from_secs(10)).await;
    assert_eq!(report.tasks_remaining, 0);
    let peak = tokio::task::spawn_blocking(move || sampler.finish())
        .await
        .map_err(|join| format!("resource sampler task failed: {join}"))??;
    let peak_mib = peak.resident_bytes / (1_024 * 1_024);
    eprintln!(
        "stress peak: RSS {peak_mib} MiB, open handles {} over {} samples",
        peak.open_handles, peak.samples
    );
    assert!(
        peak_mib < RSS_LIMIT_MIB,
        "peak RSS {peak_mib} MiB exceeds the {RSS_LIMIT_MIB} MiB cap"
    );
    assert!(
        peak.open_handles <= FD_LIMIT,
        "peak open handles {} exceed the {FD_LIMIT} cap",
        peak.open_handles
    );
    scenario_ok("scale500");
    Ok(())
}

/// The run reaches the model once with every subagent done, in one wake.
fn assert_one_delivery(journal: &str) {
    let counts = format!("{ITEMS} tasks: {ITEMS} done");
    let delivered: Vec<&str> = support::journal_records(journal, "user")
        .into_iter()
        .filter(|record| record.contains(counts.as_str()))
        .collect();
    assert_eq!(
        delivered.len(),
        1,
        "the pool report must reach the model once with every item done: \
         {delivered:?}\nJOURNAL:\n{journal}"
    );
    let wakes = support::journal_records(journal, "wake_attempt");
    assert_eq!(
        wakes.len(),
        1,
        "one delivery wake for the one run: {wakes:?}"
    );
}

fn drained_lifecycle(subscription: &mut dal_agent::HostSubscription) -> (usize, usize) {
    let mut opened = 0;
    let mut removed = 0;
    while let Some(Some(update)) = tokio::task::unconstrained(subscription.next()).now_or_never() {
        match update {
            dal_agent::HostUpdate::ChildStarted { .. } => opened += 1,
            dal_agent::HostUpdate::SessionRemoved { .. } => removed += 1,
            _ => {}
        }
    }
    (opened, removed)
}

/// Every writer's file is in the real checkout with its own content, the
/// checkout holds nothing else of the writers' making, and no worktree or
/// retained tree outlives its merge. A writer whose merge failed would leave
/// its file missing and a retained tree behind.
fn verify_writers_merged(checkout: &Path, data: &Path) -> support::TestResult<()> {
    let status = support::git_output(
        checkout,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    let mut found = BTreeSet::new();
    let mut other = Vec::new();
    for line in status.lines() {
        match line.strip_prefix("?? ") {
            Some(path) if path.starts_with("unit-") => {
                found.insert(path.to_owned());
            }
            _ => other.push(line),
        }
    }
    let expected: BTreeSet<String> = (0..WRITER_ITEMS).map(writer_file).collect();
    if found != expected {
        let missing: Vec<_> = expected.difference(&found).take(5).collect();
        let unexpected: Vec<_> = found.difference(&expected).take(5).collect();
        return Err(format!(
            "{} of {WRITER_ITEMS} writer files merged into the checkout; first missing \
             {missing:?}, unexpected {unexpected:?}; retained trees: {:?}",
            found.intersection(&expected).count(),
            retained_trees(data)
        )
        .into());
    }
    for unit in 0..WRITER_ITEMS {
        let content = fs::read_to_string(checkout.join(writer_file(unit)))?;
        // `git apply` may write platform line endings; the content is the
        // writer's own either way.
        if content.trim_end() != format!("unit {unit:04}") {
            return Err(format!("{} holds {content:?}", writer_file(unit)).into());
        }
    }
    eprintln!(
        "stress merge: {WRITER_ITEMS} writer files merged; other checkout changes: {other:?}"
    );
    let trees = support::git_output(checkout, &["worktree", "list", "--porcelain"])?;
    let listed = trees
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count();
    if listed != 1 {
        return Err(format!("git still lists {listed} worktrees after the run:\n{trees}").into());
    }
    let left = retained_trees(data);
    if !left.is_empty() {
        return Err(format!("{} task trees outlived their merge: {left:?}", left.len()).into());
    }
    Ok(())
}

/// The task trees the product keeps under `<data>/worktrees/<session>/`: a
/// merged or clean task removes its own, a retained one renames it.
fn retained_trees(data: &Path) -> Vec<PathBuf> {
    let mut left = Vec::new();
    let Ok(sessions) = fs::read_dir(data.join("worktrees")) else {
        return left;
    };
    for session in sessions.flatten() {
        let Ok(tasks) = fs::read_dir(session.path()) else {
            continue;
        };
        left.extend(tasks.flatten().map(|task| task.path()));
    }
    left
}

/// Waits until the delivery wake has ended its turn: the prompt turn plus
/// the wake turn are both closed in the parent's journal. Returns that
/// journal's text.
async fn wait_for_wake_turn_end(data: &Path, session: &str) -> support::TestResult<String> {
    let mut last = String::new();
    let waited = tokio::time::timeout(DELIVERY_WAIT, async {
        loop {
            last = read_journal(data, session).unwrap_or_default();
            if support::journal_records(&last, "turn_end").len() >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    let ended = last.matches(r#""type":"turn_end""#).count();
    waited
        .map_err(|_| format!("the pool report was not delivered in time; {ended} turns ended"))?;
    Ok(last)
}

/// Reads the journal of the parent session, found by its session id. Each
/// child session keeps its own journal beside it.
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

async fn count_fds() -> support::TestResult<usize> {
    let handles = tokio::task::spawn_blocking(support::open_handle_count)
        .await
        .map_err(|join| format!("handle counter task failed: {join}"))??;
    Ok(handles)
}

async fn rss_mib() -> support::TestResult<u64> {
    let bytes = tokio::task::spawn_blocking(support::resident_set_bytes)
        .await
        .map_err(|join| format!("RSS counter task failed: {join}"))??;
    Ok(bytes / (1_024 * 1_024))
}

// === V9 over a real loopback provider: grace-expired children, 429s ===
//
// 500 pool items (5 silent) plus 20 single-task runs (5 silent) against a
// real HTTP `openai_chat` provider on 127.0.0.1: 510 subagent jobs end
// done, 10 fail with `no report after the last turn`. Every 20th request
// answers 429 with `Retry-After` (2 s for the very first, 1 s after), and
// the product's retry path absorbs all of them without losing a report.
// The server also proves the provider gate: peak in-flight requests never
// exceed `max_concurrent_requests = 10` under 64-worker pressure.

/// The pool item tag of a silent child: it never calls its report tool.
const V9_SILENT_ITEM: &str = "silent-marker-";
/// The single-run prompt tag of a silent child.
const V9_SILENT_SINGLE: &str = "single-no-report-";
/// Pool items in the V9 loopback run; silent every 100th ending in 99.
const V9_POOL_ITEMS: usize = 500;
/// Single-task runs beside the pool; the last five stay silent.
const V9_SINGLES: usize = 20;
const V9_SILENT_SINGLES: usize = 5;
/// One in twenty requests answers 429 with `Retry-After`.
const THROTTLE_EVERY: usize = 20;
/// `Retry-After` of the very first throttle: the measurable probe.
const PROBE_RETRY_AFTER_S: u64 = 2;
/// `Retry-After` of every later throttle, the authored V9 value.
const THROTTLE_RETRY_AFTER_S: u64 = 1;
/// The provider concurrency the server must never see exceeded.
const V9_PROVIDER_LIMIT: usize = 10;
/// The chat-capable loopback model.
const V9_MODEL: &str = "openai/gpt-5.6-luna";

/// The shared state of the loopback model server.
struct Loopback {
    chats: AtomicUsize,
    throttled: AtomicUsize,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    first_throttle: std::sync::Mutex<Option<(Vec<u8>, Instant)>>,
    retry_gap: std::sync::Mutex<Option<Duration>>,
}

impl Loopback {
    fn new() -> Self {
        Self {
            chats: AtomicUsize::new(0),
            throttled: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
            first_throttle: std::sync::Mutex::new(None),
            retry_gap: std::sync::Mutex::new(None),
        }
    }
}

/// Reads one HTTP request: the target line and the `content-length` body,
/// returning the stream for the reply.
async fn read_http_request(
    stream: tokio::net::TcpStream,
) -> support::TestResult<(String, Vec<u8>, tokio::net::TcpStream)> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut length = 0usize;
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok((target, body, reader.into_inner()))
}

/// The advertised tool function names of one chat request body.
fn chat_tools(body: &sonic_rs::Value) -> Vec<String> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let Some(tools) = body.get("tools").and_then(|tools| tools.as_array()) else {
        return Vec::new();
    };
    tools
        .iter()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// The first user message text of one chat request body.
fn chat_first_user(body: &sonic_rs::Value) -> String {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return String::new();
    };
    for message in messages {
        if message.get("role").and_then(|role| role.as_str()) != Some("user") {
            continue;
        }
        let content = message.get("content");
        if let Some(text) = content.and_then(|content| content.as_str()) {
            return text.to_owned();
        }
        if let Some(parts) = content.and_then(|content| content.as_array()) {
            return parts
                .iter()
                .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("text"))
                .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    String::new()
}

/// The assistant tool calls of one chat request body, in order.
fn chat_calls(body: &sonic_rs::Value) -> Vec<(String, String)> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let mut calls = Vec::new();
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return calls;
    };
    for message in messages {
        if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
            continue;
        }
        let Some(invokes) = message.get("tool_calls").and_then(|calls| calls.as_array()) else {
            continue;
        };
        for invoke in invokes {
            let id = invoke
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or_default();
            let name = invoke
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .unwrap_or_default();
            calls.push((id.to_owned(), name.to_owned()));
        }
    }
    calls
}

/// The tool-call ids one chat request body already answers.
fn chat_answered(body: &sonic_rs::Value) -> Vec<String> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let mut answered = Vec::new();
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return answered;
    };
    for message in messages {
        if message.get("role").and_then(|role| role.as_str()) != Some("tool") {
            continue;
        }
        if let Some(id) = message.get("tool_call_id").and_then(|id| id.as_str()) {
            answered.push(id.to_owned());
        }
    }
    answered
}

/// Whether one chat request body answers a call of `tool`.
fn chat_answered_tool(body: &sonic_rs::Value, tool: &str) -> bool {
    let answered = chat_answered(body);
    chat_calls(body)
        .iter()
        .any(|(id, name)| name == tool && answered.contains(id))
}

/// One text-only SSE response.
fn sse_text(text: &str) -> String {
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{}}}}}]}}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n",
        support::json_text(text)
    )
}

/// One SSE response calling every tool in `calls` (id, wire name, args).
fn sse_calls(calls: &[(String, String, String)]) -> String {
    use std::fmt::Write as _;
    let mut events = String::new();
    for (index, (id, name, args)) in calls.iter().enumerate() {
        assert!(
            write!(
                events,
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":{},\"function\":{{\"name\":{},\"arguments\":{}}}}}]}}}}]}}\n\n",
            support::json_text(id),
            support::json_text(name),
            support::json_text(args)
            )
            .is_ok(),
            "writing to a String never fails"
        );
    }
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    assert!(
        write!(
            events,
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n"
        )
        .is_ok(),
        "writing to a String never fails"
    );
    events
}

/// The 21 `agents` calls of the V9 parent: one 500-item pool plus 20
/// single-task runs, each in one response.
fn v9_parent_calls() -> Vec<(String, String, String)> {
    let mut calls = Vec::with_capacity(1 + V9_SINGLES);
    let items = (0..V9_POOL_ITEMS)
        .map(|index| {
            if index % 100 == 99 {
                format!(r#""{V9_SILENT_ITEM}{index:04}""#)
            } else {
                format!(r#""item {index}""#)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    calls.push((
        "pool".to_owned(),
        "agents".to_owned(),
        format!(
            r#"{{"action":"run","name":"v9pool","steps":[{{"name":"map","items":[{items}],"workers":{WORKERS},"prompt":"process {{{{item}}}} and report"}}]}}"#
        ),
    ));
    for single in 0..V9_SINGLES {
        let prompt = if single >= V9_SINGLES - V9_SILENT_SINGLES {
            format!("single task {V9_SILENT_SINGLE}{single:02}, then stay silent")
        } else {
            format!("single task {single:02}, then report")
        };
        calls.push((
            format!("s{single}"),
            "agents".to_owned(),
            format!(
                r#"{{"action":"run","name":"single-{single:02}","steps":[{{"name":"solo","prompt":{}}}]}}"#,
                support::json_text(&prompt)
            ),
        ));
    }
    calls
}

/// One loopback model response for a parsed chat body.
fn v9_response(body: &sonic_rs::Value) -> String {
    let tools = chat_tools(body);
    if tools.iter().any(|tool| tool == REPORT_TOOL_NAME) {
        let first = chat_first_user(body);
        // A silent child answers in text twice (turn plus grace) and never
        // calls its report tool; the grace expiry then fails it loudly.
        if first.contains(V9_SILENT_ITEM) || first.contains(V9_SILENT_SINGLE) {
            return sse_text("nothing to report");
        }
        if chat_answered_tool(body, REPORT_TOOL_NAME) {
            return sse_text("reported");
        }
        return sse_calls(&[(
            "r1".to_owned(),
            REPORT_TOOL_NAME.to_owned(),
            REPORT_CALL.to_owned(),
        )]);
    }
    if chat_answered_tool(body, "agents") {
        return sse_text("started");
    }
    sse_calls(&v9_parent_calls())
}

/// Serves one loopback connection: throttles every 20th chat request with
/// `Retry-After`, else answers after seeded 20-200 ms latency.
async fn serve_loopback(
    stream: tokio::net::TcpStream,
    state: &Arc<Loopback>,
) -> support::TestResult<()> {
    use tokio::io::AsyncWriteExt;
    let (target, body, mut stream) = read_http_request(stream).await?;
    if !target.contains("chat/completions") {
        let response = "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }
    let arrival = state.chats.fetch_add(1, Ordering::SeqCst);
    if arrival.is_multiple_of(THROTTLE_EVERY) {
        state.throttled.fetch_add(1, Ordering::SeqCst);
        let retry_after = if arrival == 0 {
            PROBE_RETRY_AFTER_S
        } else {
            THROTTLE_RETRY_AFTER_S
        };
        {
            let mut first = state
                .first_throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if first.is_none() {
                *first = Some((body.clone(), Instant::now()));
            }
        }
        let payload = r#"{"error":{"type":"rate_limit_exceeded","message":"loopback throttle"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: {retry_after}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
            payload.len()
        );
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }
    // The client's retry of the probe throttle: same bytes, ≥2 s later.
    // Any other request without a body match leaves the probe alone.
    {
        let first = state
            .first_throttle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((probe, at)) = first.as_ref()
            && *probe == body
        {
            let mut gap = state
                .retry_gap
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if gap.is_none() {
                *gap = Some(at.elapsed());
            }
        }
    }
    let live = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak_in_flight.fetch_max(live, Ordering::SeqCst);
    // Seeded 20-200 ms provider latency, as the authored V9 fixture.
    let latency = 20 + (arrival.wrapping_mul(37) % 181) as u64;
    tokio::time::sleep(Duration::from_millis(latency)).await;
    let parsed = sonic_rs::from_slice::<sonic_rs::Value>(&body)
        .map_err(|error| format!("loopback request is not JSON: {error}"))?;
    let events = v9_response(&parsed);
    // Leave the in-flight count before the final write: the client releases
    // its permit on receipt, so decrementing first can only undercount the
    // peak, never hide an overrun.
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
        events.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

#[test]
fn stress_v9_silent_and_throttled_over_loopback_provider() -> support::TestResult<()> {
    if std::env::var_os("DALGONA_ORCH_STRESS").is_none() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(v9_loopback_scenario())
}

/// The V9 loopback run: 520 subagents over a real HTTP provider with real
/// 429 `Retry-After` answers, 510 done and 10 grace-expired.
async fn v9_loopback_scenario() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-v9-loop-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(Loopback::new());
    let server = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let _ = serve_loopback(stream, &state).await;
                });
            }
        }
    });
    let user = format!(
        "model = \"{V9_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n\n[plugin.orchestration.agents]\nmax_runs = 64\n\n[providers.openai]\nbase_url = \"http://127.0.0.1:{port}\"\napi = \"openai_chat\"\nmax_concurrent_requests = {V9_PROVIDER_LIMIT}\n"
    );
    support::grant_bundled_extensions(&data, &workspace, &user, GATE_CLIENT).await?;
    let product = support::build_product(data.clone(), Some(&user))?;
    let mut vars = std::collections::BTreeMap::new();
    vars.insert(
        std::ffi::OsString::from("OPENAI_API_KEY"),
        std::ffi::OsString::from("sk-test"),
    );
    let env = dal_agent::Env {
        vars,
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, support::host_config(&data, &user)?, env).await?;
    let agent = host
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(workspace.clone())?,
                name: None,
            },
            dal_core::ClientId::new(GATE_CLIENT),
        )
        .await?;
    let session = agent
        .view(dal_core::PageReq::default())?
        .session
        .id
        .to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let slow = spawn_slow_reader(&agent, &stop)?;
    let sampler = support::PeakSampler::start(SAMPLE_PERIOD);
    agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![Part::Text {
                text: "run the v9 pool and singles".into(),
            }],
        })
        .await?;
    let journal = wait_for_v9_quiet(&data, &session).await?;
    stop.store(true, Ordering::Release);
    let ledger = slow
        .await
        .map_err(|join| format!("slow reader failed: {join}"))?;
    eprintln!("v9-loopback subscriber: {}", ledger.summary());
    ledger.verdict()?;
    check_v9_notices(&journal)?;
    assert_v9_provider_contract(&state).await?;
    let report = host.shutdown(Duration::from_secs(10)).await;
    if report.tasks_remaining != 0 {
        return Err("tasks outlived the v9 loopback shutdown".into());
    }
    let peak = tokio::task::spawn_blocking(move || sampler.finish())
        .await
        .map_err(|join| format!("resource sampler task failed: {join}"))??;
    let peak_mib = peak.resident_bytes / (1_024 * 1_024);
    if peak_mib >= RSS_LIMIT_MIB {
        return Err(format!("peak RSS {peak_mib} MiB exceeds the {RSS_LIMIT_MIB} MiB cap").into());
    }
    if peak.open_handles > FD_LIMIT {
        return Err(format!(
            "peak handles {} exceed the {FD_LIMIT} cap",
            peak.open_handles
        )
        .into());
    }
    server.abort();
    scenario_ok("v9-loopback");
    Ok(())
}

/// Asserts the deterministic throttle math, probe `Retry-After` gap, provider
/// concurrency peak, and steady-state resource caps of the V9 loopback run.
async fn assert_v9_provider_contract(state: &Loopback) -> support::TestResult<()> {
    // The throttle math is deterministic: arrivals 0, 20, 40, ... fail.
    let chats = state.chats.load(Ordering::SeqCst);
    let throttled = state.throttled.load(Ordering::SeqCst);
    let want_throttled = chats.div_ceil(THROTTLE_EVERY);
    if throttled != want_throttled {
        return Err(
            format!("{throttled} throttles of {chats} chats, expected {want_throttled}").into(),
        );
    }
    if throttled < 50 {
        return Err(format!("only {throttled} throttles: the 5% rate never bit").into());
    }
    let gap = state
        .retry_gap
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ok_or("the probe throttle was never retried")?;
    if gap < Duration::from_secs(PROBE_RETRY_AFTER_S) {
        return Err(
            format!("the probe retry came back after {gap:?}, before its Retry-After").into(),
        );
    }
    let peak = state.peak_in_flight.load(Ordering::SeqCst);
    if peak > V9_PROVIDER_LIMIT {
        return Err(
            format!("provider in-flight peaked at {peak}, above {V9_PROVIDER_LIMIT}").into(),
        );
    }
    if peak < 2 {
        return Err(format!("provider in-flight peaked at {peak}: no pressure was proven").into());
    }
    eprintln!(
        "v9-loopback provider: {chats} chats, {throttled} throttled, probe gap {gap:?}, peak in-flight {peak}"
    );
    let rss = rss_mib().await?;
    if rss >= RSS_LIMIT_MIB {
        return Err(format!("RSS {rss} MiB exceeds the {RSS_LIMIT_MIB} MiB cap").into());
    }
    let fds = count_fds().await?;
    if fds > FD_LIMIT {
        return Err(format!("open fds {fds} exceed the {FD_LIMIT} cap").into());
    }
    Ok(())
}

/// Waits for all 21 run notices to land and stay put: the notice state
/// holds for 2 s. Counts notices, not wakes: several runs may share one
/// wake, but every run renders its own header exactly once.
async fn wait_for_v9_quiet(data: &Path, session: &str) -> support::TestResult<String> {
    let mut last = String::new();
    let mut stable_since = Instant::now();
    let mut was_ok = false;
    let waited = tokio::time::timeout(DELIVERY_WAIT, async {
        loop {
            last = read_journal(data, session).unwrap_or_default();
            let ok = v9_notices_ok(&last).is_ok();
            if ok && !was_ok {
                was_ok = true;
                stable_since = Instant::now();
            } else if !ok {
                was_ok = false;
            }
            if was_ok && stable_since.elapsed() > Duration::from_secs(2) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    waited.map_err(|_| "the 21 run notices never all landed".to_string())?;
    Ok(last)
}

/// The header section of one run: from its `"label"` to the next 120
/// characters, which always cover `done/failed in … · N tasks: counts`.
/// A batched wake shares one record across runs, so counts are attributed
/// per section, never per record.
fn run_section(line: &str, label: &str) -> Option<String> {
    let at = line.find(label)?;
    Some(line.get(at..)?.chars().take(label.len() + 120).collect())
}

/// The 21 notices without the log line: the quiet-wait polls this.
fn v9_notices_ok(journal: &str) -> support::TestResult<()> {
    let users = support::journal_records(journal, "user");
    let pool: Vec<String> = users
        .iter()
        .filter_map(|line| run_section(line, "v9pool"))
        .collect();
    if pool.len() != 1 {
        return Err(format!("the pool notice landed {} times", pool.len()).into());
    }
    if !pool[0].contains("495 done") || !pool[0].contains("5 failed") {
        return Err("the pool notice is not 495 done / 5 failed".into());
    }
    for single in 0..V9_SINGLES {
        let label = format!("single-{single:02}");
        let hits: Vec<String> = users
            .iter()
            .filter_map(|line| run_section(line, &label))
            .collect();
        if hits.len() != 1 {
            return Err(format!("{label} landed {} times", hits.len()).into());
        }
        let silent = single >= V9_SINGLES - V9_SILENT_SINGLES;
        let want = if silent { "1 failed" } else { "1 done" };
        if !hits[0].contains(want) {
            return Err(format!("{label} is not {want}").into());
        }
    }
    let no_report: usize = users
        .iter()
        .map(|line| line.matches("no report after the last turn").count())
        .sum();
    if no_report != 5 + V9_SILENT_SINGLES {
        return Err(format!("{no_report} no-report failures, expected 10").into());
    }
    Ok(())
}

/// The 21 notices: the pool at 495 done / 5 failed, 15 singles done, 5
/// singles failed with the exact grace text, every header exactly once.
/// Only `user` records count: the assistant `agents` calls and their tool
/// results carry the same labels, so whole-journal counts never settle.
fn check_v9_notices(journal: &str) -> support::TestResult<()> {
    v9_notices_ok(journal)?;
    eprintln!("v9-loopback notices: pool 495 done / 5 failed, 15 singles done, 5 silent failed");
    Ok(())
}
