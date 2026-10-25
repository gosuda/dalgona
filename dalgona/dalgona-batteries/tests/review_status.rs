// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#![expect(
    clippy::disallowed_methods,
    reason = "this boundary test intentionally drives the real git executable"
)]

//! The review battery over a real host and real git: the status cap is
//! enforced after git output crosses the boundary, and the shared `review`
//! status row is busy exactly while a round holds its reviewer request.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, Block, CancelScope, Caps, ClientId, Command as SessionCommand, Config, ConfigProduct,
    EntryKind, Expect, ExtState, ExtStatus, JournalPart, ModelId, ModelRequest, ModelRoute,
    PageReq, Part, Record, Reply, ServiceSet, ThinkingLevel, TurnId, UpdateKind, Workspace,
};
use dal_store::Store;
use dalgona_batteries::review::{ReviewConfig, review};
use tokio::sync::Notify;

const STATUS_LIMIT: usize = 262_144;
const WAIT: Duration = Duration::from_secs(30);
const PARENT_FOLLOW_UP_TEXT: &str = "parent turn completed";
const STATUS_TOO_LARGE_TEXT: &str =
    "workspace status exceeds 262144 bytes; commit or stash unrelated changes";
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;
/// The session route the scripted provider serves; the held reviewer
/// forwards to it once released.
const SESSION_MODEL: &str = "openai/gpt-6-luna";
/// The synthetic reviewer route whose requests the test holds open.
const HELD_REVIEWER: &str = "held/reviewer";
/// The exact status payload of round 1 under the default cap of 3 rounds.
const RUNNING_PAYLOAD: &str = r#"{"state":"running","round":1,"max_rounds":3}"#;

type TestResult = Result<(), Box<dyn StdError>>;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let path = std::env::temp_dir().join(format!(
            "dalgona-review-status-{}",
            dal_core::SessionId::new_v7()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn StdError>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

fn create_oversized_status_workspace(scratch: &Scratch) -> Result<PathBuf, Box<dyn StdError>> {
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    run_git(&workspace, &["init", "--quiet"])?;
    run_git(
        &workspace,
        &["config", "user.email", "review-test@example.invalid"],
    )?;
    run_git(&workspace, &["config", "user.name", "review status test"])?;
    run_git(
        &workspace,
        &["commit", "--allow-empty", "--quiet", "-m", "baseline"],
    )?;

    for index in 0..1_200 {
        let filename = format!("status-overflow-{index:04}-{}", "x".repeat(220));
        fs::write(workspace.join(filename), b"")?;
    }

    let status = run_git(&workspace, &["status", "--short"])?;
    assert!(
        status.len() > STATUS_LIMIT,
        "fixture status must exceed the review cap: {} bytes",
        status.len()
    );
    let diff = run_git(&workspace, &["diff", "HEAD"])?;
    assert!(
        diff.is_empty(),
        "fixture must overflow status, not diff (diff was {} bytes)",
        diff.len()
    );
    Ok(workspace)
}

fn review_call_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"review-call","name":"review"}},{{"type":"tool_calls_done","calls":[{{"id":"review-call","name":"review","args":{{"kind":"parsed","value":{{}}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

fn parent_follow_up_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{PARENT_FOLLOW_UP_TEXT}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

async fn start_review_session(
    scratch: &Scratch,
    workspace: &Path,
) -> Result<(Host, Agent, Subscription), Box<dyn StdError>> {
    let data = scratch.path.join("data");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        format!("{}\n{}\n", review_call_step(), parent_follow_up_step()),
    )?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
    let extension = review(ReviewConfig::default())?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.to_owned(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_owned())?,
                name: None,
            },
            ClientId::new("review-status-overflow"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok((host, agent, subscription))
}

async fn wait_for_turn_end(agent: &Agent, subscription: &mut Subscription) -> TestResult {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    agent.answer(request.id, Answer::ApproveForSession).await?;
                }
                UpdateKind::TurnEnded { .. } => return Ok(()),
                _ => {}
            }
        }
        Err("subscription closed before the parent turn ended".into())
    })
    .await?
}

fn review_error_text(view: &dal_core::View) -> Option<&str> {
    view.entries.items.iter().find_map(|entry| {
        let EntryKind::ToolResult {
            name,
            error: true,
            parts,
            ..
        } = &entry.kind
        else {
            return None;
        };
        if name.as_ref() != "review" {
            return None;
        }
        parts.iter().find_map(|part| match part {
            JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
    })
}

fn parent_completion_count(view: &dal_core::View) -> usize {
    view.entries
        .items
        .iter()
        .filter(|entry| matches!(&entry.kind, EntryKind::Assistant { .. }))
        .count()
}

fn parent_completed(view: &dal_core::View) -> bool {
    view.entries.items.iter().any(|entry| {
        let EntryKind::Assistant { content, .. } = &entry.kind else {
            return false;
        };
        content.iter().any(
            |block| matches!(block, Block::Text { text, .. } if text.as_ref() == PARENT_FOLLOW_UP_TEXT),
        )
    })
}

#[tokio::test]
async fn real_git_status_overflow_rejects_before_reviewer_completion_and_closes_cleanly()
-> TestResult {
    let scratch = Scratch::new()?;
    let workspace = create_oversized_status_workspace(&scratch)?;
    let (host, agent, mut subscription) = start_review_session(&scratch, &workspace).await?;
    let session = agent.session();

    let reply = agent
        .submit(SessionCommand::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "review the current changes".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(format!("prompt was not accepted: {reply:?}").into());
    }
    wait_for_turn_end(&agent, &mut subscription).await?;

    let view = agent.view(PageReq::default())?;
    assert_eq!(
        review_error_text(&view),
        Some(STATUS_TOO_LARGE_TEXT),
        "the actual review tool must return the actionable status-cap error"
    );
    assert_eq!(
        parent_completion_count(&view),
        2,
        "the fixture's two completions belong to the parent turn; a reviewer completion would consume the follow-up"
    );
    assert!(
        parent_completed(&view),
        "the parent turn must consume its distinct follow-up completion"
    );
    assert!(
        agent
            .poll_status()
            .await?
            .iter()
            .all(|status| status.ext.as_ref() != "review"),
        "review status must be idle (no live review row) after rejection"
    );

    host.close(session).await?;
    let store = Store::new(
        scratch.path.join("data"),
        Workspace::new(workspace)?,
        dal_core::Product::Dal,
    );
    let (journal, _) = store.open_session(session).await?;
    assert!(
        journal.records().iter().all(|record| {
            !matches!(
                record,
                Record::Ext { ext, kind, .. }
                    if ext.as_ref() == "review" && kind.as_ref() == "review"
            )
        }),
        "the closed parent session must persist no review record"
    );
    Ok(())
}

/// A workspace with one committed file modified in the worktree, so a round
/// has a diff to send to the reviewer.
fn create_changed_workspace(scratch: &Scratch) -> Result<PathBuf, Box<dyn StdError>> {
    let workspace = scratch.path.join("workspace");
    fs::create_dir_all(&workspace)?;
    run_git(&workspace, &["init", "--quiet"])?;
    run_git(
        &workspace,
        &["config", "user.email", "review-test@example.invalid"],
    )?;
    run_git(&workspace, &["config", "user.name", "review status test"])?;
    fs::write(workspace.join("tracked.txt"), b"before\n")?;
    run_git(&workspace, &["add", "tracked.txt"])?;
    run_git(&workspace, &["commit", "--quiet", "-m", "baseline"])?;
    fs::write(workspace.join("tracked.txt"), b"after\n")?;
    Ok(workspace)
}

/// The reviewer's clean reply, served by the scripted provider after the
/// held reviewer forwards its request.
fn reviewer_clean_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{{\"verdict\":\"clean\",\"findings\":[],\"summary\":\"looks good\"}}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

/// The handshake between the test and the held reviewer model.
#[derive(Default)]
struct Hold {
    /// Signalled once the reviewer request reaches the model handler.
    entered: Notify,
    /// Signalled by the test to let the held request forward.
    release: Notify,
}

/// A synthetic reviewer model that holds each request until released, then
/// forwards it to the scripted session route. A cancelled round drops the
/// held future before it forwards.
struct HeldReviewer {
    hold: Arc<Hold>,
}

impl ModelHandler for HeldReviewer {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.hold.entered.notify_one();
            self.hold.release.notified().await;
            request.model = ModelRoute::from_id(SESSION_MODEL);
            cx.forward(request, &[]).await
        })
    }
}

fn held_reviewer_extension(hold: Arc<Hold>) -> Result<Extension, Box<dyn StdError>> {
    Ok(
        ExtensionBuilder::new("held-reviewer", "0.1.0", ServiceSet::EMPTY)?
            .model(ModelRecord {
                id: ModelId::parse(HELD_REVIEWER)?,
                caps: Caps {
                    context_window: Some(100_000),
                    thinking: Box::new([ThinkingLevel::Off]),
                    tool_use: true,
                    image_input: false,
                    custom_grammar: false,
                },
                handler: Arc::new(HeldReviewer { hold }),
                export: None,
            })
            .build()?,
    )
}

/// One host with the review battery reviewing through the held model, a
/// reviewing session, and a second open session that never reviews.
struct HeldReview {
    host: Host,
    agent: Agent,
    other: Agent,
    subscription: Subscription,
    hold: Arc<Hold>,
    /// Every `review` status update the reviewing session published, in order.
    review_updates: Vec<ExtStatus>,
}

impl HeldReview {
    async fn start(
        scratch: &Scratch,
        workspace: &Path,
        steps: &[String],
    ) -> Result<Self, Box<dyn StdError>> {
        let data = scratch.path.join("data");
        fs::create_dir_all(&data)?;
        let fixture = data.join("script.jsonl");
        fs::write(&fixture, steps.join("\n") + "\n")?;
        let user = format!(
            "approval = \"all\"\nmodel = \"{SESSION_MODEL}\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        );
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
        let hold = Arc::new(Hold::default());
        let product = Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![
                review(ReviewConfig {
                    reviewer_model: HELD_REVIEWER.into(),
                    ..ReviewConfig::default()
                })?,
                held_reviewer_extension(Arc::clone(&hold))?,
            ],
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: workspace.to_owned(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await?;
        let root = Workspace::new(workspace.to_owned())?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace: root.clone(),
                    name: None,
                },
                ClientId::new("review-status-held"),
            )
            .await?;
        let other = host
            .open(
                SessionRef::New {
                    workspace: root,
                    name: None,
                },
                ClientId::new("review-status-bystander"),
            )
            .await?;
        let subscription = agent.subscribe(None)?;
        Ok(Self {
            host,
            agent,
            other,
            subscription,
            hold,
            review_updates: Vec::new(),
        })
    }

    async fn prompt(&self) -> Result<TurnId, Box<dyn StdError>> {
        let reply = self
            .agent
            .submit(SessionCommand::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "review the current changes".into(),
                }],
            })
            .await?;
        let Reply::Accepted { turn, .. } = reply else {
            return Err(format!("prompt was not accepted: {reply:?}").into());
        };
        Ok(turn)
    }

    /// Takes one update: approves every grant request through the real
    /// grant path, records `review` statuses, and reports whether the turn
    /// ended.
    async fn step(&mut self) -> Result<bool, Box<dyn StdError>> {
        let Some(delivery) = self.subscription.next().await else {
            return Err("subscription closed".into());
        };
        let Delivery::Update(update) = delivery else {
            return Ok(false);
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                self.agent
                    .answer(request.id, Answer::ApproveForSession)
                    .await?;
            }
            UpdateKind::ExtStatus(status) if status.ext.as_ref() == "review" => {
                self.review_updates.push(status.clone());
            }
            UpdateKind::TurnEnded { .. } => return Ok(true),
            _ => {}
        }
        Ok(false)
    }

    /// Drives the turn until the reviewer request is held by the model.
    async fn until_reviewer_holds(&mut self) -> TestResult {
        let hold = Arc::clone(&self.hold);
        tokio::time::timeout(WAIT, async {
            let entered = hold.entered.notified();
            tokio::pin!(entered);
            loop {
                tokio::select! {
                    () = &mut entered => return Ok(()),
                    ended = self.step() => {
                        if ended? {
                            return Err("the turn ended before the reviewer request was held".into());
                        }
                    }
                }
            }
        })
        .await?
    }

    async fn until_turn_end(&mut self) -> TestResult {
        tokio::time::timeout(WAIT, async {
            while !self.step().await? {}
            Ok(())
        })
        .await?
    }

    /// While the reviewer request is held, the reviewing session's `review`
    /// row is busy with the running payload, and the other session's is not.
    async fn assert_busy(&self) -> TestResult {
        let polled = self.agent.poll_status().await?;
        let row = polled
            .iter()
            .find(|status| status.ext.as_ref() == "review")
            .ok_or("no review status row while the reviewer request is held")?;
        assert_eq!(
            row,
            &ExtStatus {
                ext: "review".into(),
                state: ExtState::Busy,
                text: Some(RUNNING_PAYLOAD.into()),
            },
            "a held review round is work in flight: busy, carrying its payload"
        );
        assert!(
            !self.agent.is_quiet().await?,
            "the reviewing session is not quiet while its round runs"
        );
        assert!(
            self.agent.ext_status().contains(row),
            "the published snapshot a late client seeds from carries the busy row"
        );
        assert!(
            self.other
                .poll_status()
                .await?
                .iter()
                .all(|status| status.ext.as_ref() != "review"),
            "another session's review row stays quiet"
        );
        assert!(
            self.other.is_quiet().await?,
            "another session stays quiet while a round runs elsewhere"
        );
        Ok(())
    }

    /// After the round ends, both sessions are quiet, and the update stream
    /// carried the busy row and then the quiet reset.
    async fn assert_quiet_after(&mut self) -> TestResult {
        assert!(
            self.agent
                .poll_status()
                .await?
                .iter()
                .all(|status| status.ext.as_ref() != "review"),
            "the review row is quiet once the round ends"
        );
        assert!(
            self.agent.is_quiet().await?,
            "the reviewing session is quiet once the round ends"
        );
        assert!(
            self.agent
                .ext_status()
                .iter()
                .all(|status| status.ext.as_ref() != "review"),
            "the published snapshot drops the review row once the round ends"
        );
        assert!(
            self.other.is_quiet().await?,
            "another session stays quiet after the round ends"
        );
        let quiet = ExtStatus {
            ext: "review".into(),
            state: ExtState::Quiet,
            text: None,
        };
        tokio::time::timeout(WAIT, async {
            while self.review_updates.last() != Some(&quiet) {
                self.step().await?;
            }
            Ok::<(), Box<dyn StdError>>(())
        })
        .await??;
        assert!(
            self.review_updates.iter().any(|status| *status
                == ExtStatus {
                    ext: "review".into(),
                    state: ExtState::Busy,
                    text: Some(RUNNING_PAYLOAD.into()),
                }),
            "the update stream carried the busy row: {:?}",
            self.review_updates
        );
        Ok(())
    }

    async fn close(self) -> TestResult {
        self.host.close(self.other.session()).await?;
        self.host.close(self.agent.session()).await?;
        Ok(())
    }
}

fn review_result_text(view: &dal_core::View) -> Option<&str> {
    view.entries.items.iter().find_map(|entry| {
        let EntryKind::ToolResult {
            name,
            error: false,
            parts,
            ..
        } = &entry.kind
        else {
            return None;
        };
        if name.as_ref() != "review" {
            return None;
        }
        parts.iter().find_map(|part| match part {
            JournalPart::Text { text } => Some(text.as_ref()),
            _ => None,
        })
    })
}

#[tokio::test]
async fn review_status_is_busy_while_the_reviewer_runs_and_quiet_after_completion() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = create_changed_workspace(&scratch)?;
    let mut rig = HeldReview::start(
        &scratch,
        &workspace,
        &[
            review_call_step(),
            reviewer_clean_step(),
            parent_follow_up_step(),
        ],
    )
    .await?;
    rig.prompt().await?;
    rig.until_reviewer_holds().await?;

    rig.assert_busy().await?;

    rig.hold.release.notify_one();
    rig.until_turn_end().await?;
    let view = rig.agent.view(PageReq::default())?;
    let result = review_result_text(&view).ok_or("the review round returned no result")?;
    assert!(
        result.contains("clean. No findings."),
        "the released round completes through the reviewer: {result}"
    );
    assert!(
        parent_completed(&view),
        "the parent turn consumes its follow-up after the round"
    );

    rig.assert_quiet_after().await?;
    rig.close().await
}

#[tokio::test]
async fn review_status_returns_quiet_when_the_held_review_turn_is_cancelled() -> TestResult {
    let scratch = Scratch::new()?;
    let workspace = create_changed_workspace(&scratch)?;
    let mut rig = HeldReview::start(&scratch, &workspace, &[review_call_step()]).await?;
    let turn = rig.prompt().await?;
    rig.until_reviewer_holds().await?;

    rig.assert_busy().await?;

    rig.agent
        .submit(SessionCommand::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await?;
    rig.until_turn_end().await?;
    assert!(
        review_result_text(&rig.agent.view(PageReq::default())?).is_none(),
        "a cancelled round returns no successful review result"
    );

    rig.assert_quiet_after().await?;
    rig.close().await
}
