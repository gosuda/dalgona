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

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_core::{
    Caps, ContextItem, ModelId, ModelRequest, ModelToolSpec, Part, RawJson, ServiceSet,
    ThinkingLevel, Usage,
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
                        .expect("registered extension has a code identity"),
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
