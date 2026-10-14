// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion scale gate: a 500-item pool runs
//! through the real host, every item reports done, and the process holds
//! its resource contract (RSS under 256 MiB, open fds under 2048).
//!
//! Set `DALGONA_ORCH_STRESS=1` to run; the default suite skips the minutes
//! long fan-out, matching the contract's stress-gate opt-in.
//!
//! The model boundary is deterministic without touching the shared replay
//! fixture: a registered synthetic model (the public `ModelHandler`
//! extension API, as `wires-08` uses it) answers each request from its own
//! context. A child whose context carries the pool item reports `done`
//! through `orchestration__report`; a request that has already seen an
//! `agents` tool result gets the closing text. The shared FIFO replay
//! fixture cannot give that, because one queue serves every session in
//! script order. The 64-worker pool and every delivery, wake, and resource
//! assertion are unchanged.
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "gate reads its opt-in flag and scratch dirs from the environment"
)]

use dalgona_gates::support;

use futures::FutureExt as _;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
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

const ITEMS: usize = 500;
const RSS_LIMIT_MIB: u64 = 256;
const FD_LIMIT: usize = 2_048;
const DELIVERY_WAIT: Duration = Duration::from_secs(600);

const POOL_MODEL: &str = "stress-gate/pool";
const ROOT_MODEL: &str = "stress-gate/root";
const REPORT_TOOL_NAME: &str = "orchestration__report";

/// The model-facing `agents` call gate 09 submits: a 500-item pool over 64
/// workers.
fn pool_call() -> String {
    let items = (0..ITEMS)
        .map(|index| format!(r#""item {index}""#))
        .collect::<Vec<_>>()
        .join(",");
    r#"{"action":"run","name":"pool","steps":[{"name":"map","items":"#.to_owned()
        + "["
        + &items
        + r#"],"workers":64,"prompt":"process {{item}} and report"}]}"#
}

const REPORT_CALL: &str = r#"{"status":"done","report":"processed"}"#;

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
            // A child step sees exactly the report tool. Its first request
            // reports done; the request after the report result ends the
            // child's turn.
            if child_reports(request.tools.as_ref()) {
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

/// Whether the visible tool surface contains the pool step's report tool:
/// only such a child request can report.
fn child_reports(tools: &[ModelToolSpec]) -> bool {
    tools
        .iter()
        .any(|tool| tool.name.as_ref() == REPORT_TOOL_NAME)
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
    fs::create_dir_all(&workspace)?;
    let user = format!("model = \"{ROOT_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let started = Instant::now();
        grant_bundled_extensions(&data, &workspace, &user).await?;
        let product = support::build_product(data.clone(), Some(&user))?;
        let mut product = product;
        product.extensions.push(pool_extension()?);
        let env = dal_agent::Env {
            vars: std::collections::BTreeMap::default(),
            cwd: workspace.clone(),
            sandbox_helper: None,
        };
        let host = dal_agent::Host::start(product, host_config(&data, &user)?, env).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace)?,
                    name: None,
                },
                dal_core::ClientId::new("stress-gate"),
            )
            .await?;
        let session = agent
            .view(dal_core::PageReq::default())?
            .session
            .id
            .to_string();
        let mut lifecycle = host.subscribe();
        let rss_before_prompt = rss_mib()?;
        agent
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![Part::Text {
                    text: "run the pool".into(),
                }],
            })
            .await?;
        let journal = wait_for_wake_turn_end(&data, &session).await?;
        let elapsed = started.elapsed();

        let counts = format!("{ITEMS} tasks: {ITEMS} done");
        let delivered: Vec<&str> = support::journal_records(&journal, "user")
            .into_iter()
            .filter(|record| record.contains(&counts))
            .collect();
        assert_eq!(
            delivered.len(),
            1,
            "the pool report must reach the model once with every item done: \
             {delivered:?}\nJOURNAL:\n{journal}"
        );
        let wakes = support::journal_records(&journal, "wake_attempt");
        assert_eq!(
            wakes.len(),
            1,
            "one delivery wake for the one run: {wakes:?}"
        );

        let rss_post_wake = rss_mib()?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let rss_post_settle = rss_mib()?;
        let rss_mib = rss_post_settle.max(rss_post_wake);
        let (opened, removed) = drained_lifecycle(&mut lifecycle);
        eprintln!(
            "stress resources: before={rss_before_prompt} MiB, wake={rss_post_wake} MiB, \
             settled={rss_post_settle} MiB, children opened={opened}, removed={removed}"
        );
        assert!(
            rss_mib < RSS_LIMIT_MIB,
            "RSS {rss_mib} MiB exceeds the {RSS_LIMIT_MIB} MiB cap \
             (before-prompt {rss_before_prompt}, post-wake {rss_post_wake}, post-settle {rss_post_settle})"
        );
        let fds = count_fds()?;
        assert!(fds <= FD_LIMIT, "open fds {fds} exceed the {FD_LIMIT} cap");
        let _ = (fds, elapsed);
        let report = host.shutdown(Duration::from_secs(10)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
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

/// Grants every bundled extension the services it declares, as a person
/// would on first use, so no grant question interrupts the run.
async fn grant_bundled_extensions(
    data: &Path,
    workspace: &Path,
    user: &str,
) -> support::TestResult<()> {
    let product = support::build_product(data.to_path_buf(), Some(user))?;
    for extension in &product.extensions {
        let services = extension.inject().capabilities();
        if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
            continue;
        }
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(
                dal_agent::ext::grants::GrantKey::for_extension(extension, workspace)
                        .expect("registered extension has a code identity"),
                dal_core::ClientId::new("stress-gate"),
                dal_core::Timestamp::now(),
            )
            .await?;
    }
    Ok(())
}

/// Loads the layered config the host start needs, from the same data root
/// and user TOML the product build used.
fn host_config(
    data: &Path,
    user: &str,
) -> Result<dal_core::Config, Box<dyn std::error::Error + Send + Sync>> {
    let factory = dalgona::product();
    Ok(dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        data,
        factory.defaults,
        Some(user),
    )?)
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

fn count_fds() -> support::TestResult<usize> {
    let count = std::fs::read_dir("/proc/self/fd")?.count();
    Ok(count)
}

fn rss_mib() -> support::TestResult<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or("no VmRSS in /proc/self/status")?;
    let kib: u64 = line
        .split_whitespace()
        .nth(1)
        .ok_or("VmRSS has no value")?
        .parse()
        .map_err(|error| format!("VmRSS parse: {error}"))?;
    Ok(kib / 1_024)
}
