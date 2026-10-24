//! Integration tests for branch switching, lifecycle Settling view,
//! and reopen projection consistency.

#![expect(
    clippy::expect_used,
    reason = "fixed fixture values and test observations fail loudly"
)]
#![expect(
    clippy::panic,
    reason = "unexpected test replies and closed subscriptions fail loudly"
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::BeforeTurn;
use dal_core::{
    CallId, Caps, ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect,
    FileChange, JournalPart, ModelId, ModelRequest, PageReq, Part, Record, Reply, ServiceSet,
    SessionId, Stop, ThinkingLevel, Timestamp, TurnId, TurnState, Workspace,
};
use dal_provider::{EventStream, ProviderError, StopReason, StreamEvent};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(20);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(value).expect("nonzero fixture id"))
}

struct SyntheticModel {
    input_tokens: u64,
    output_tokens: u64,
}

impl ModelHandler for SyntheticModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let input_tokens = self.input_tokens;
        let output_tokens = self.output_tokens;
        Box::pin(async move {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "synthetic reply".into(),
                }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
                Ok(StreamEvent::Usage {
                    usage: dal_core::Usage {
                        input_tokens,
                        cached_input_tokens: 0,
                        output_tokens,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                        premium_requests: None,
                    },
                }),
                Ok(StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                }),
            ];
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

fn synthetic_extension(input_tokens: u64, output_tokens: u64) -> Extension {
    let model = SyntheticModel {
        input_tokens,
        output_tokens,
    };
    ExtensionBuilder::new("synth", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .model(ModelRecord {
            id: ModelId::parse("synth/test-model").expect("model id"),
            caps: Caps {
                context_window: Some(128_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(model),
            export: None,
        })
        .build()
        .expect("synth extension")
}

struct TestHarness {
    _tmp: tempfile::TempDir,
    host: Host,
    workspace: Workspace,
    data_dir: std::path::PathBuf,
}

impl TestHarness {
    fn store(&self) -> dal_store::Store {
        dal_store::Store::new(
            self.data_dir.clone(),
            self.workspace.clone(),
            dal_core::Product::Dal,
        )
    }

    async fn new_agent(&self, client: &'static str) -> TestResult<Agent> {
        Ok(self
            .host
            .open(
                SessionRef::New {
                    workspace: self.workspace.clone(),
                    name: None,
                },
                ClientId::new(client),
            )
            .await?)
    }

    async fn resume_agent(&self, session: SessionId, client: &'static str) -> TestResult<Agent> {
        Ok(self
            .host
            .open(
                SessionRef::Resume {
                    key: session.to_string().into(),
                    workspace: self.workspace.clone(),
                },
                ClientId::new(client),
            )
            .await?)
    }
}

async fn start_harness(extensions: Vec<Extension>) -> TestResult<TestHarness> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;

    let config_text = "approval = \"all\"\nmodel = \"synth/test-model\"\n";
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text))?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(workspace_dir)?;
    Ok(TestHarness {
        _tmp: tmp,
        host,
        workspace,
        data_dir: data,
    })
}

async fn wait_for_turn_start(subscription: &mut Subscription, expected_turn: TurnId) {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnStarted { turn, .. } = update.kind
                && turn == expected_turn
            {
                return;
            }
        }
        panic!("subscription ended before TurnStarted for {expected_turn}");
    })
    .await
    .expect("turn started within deadline");
}

async fn wait_for_turn_end(subscription: &mut Subscription, expected_turn: TurnId) -> Stop {
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && let dal_core::UpdateKind::TurnEnded { turn, stop } = update.kind
                && turn == expected_turn
            {
                return stop;
            }
        }
        panic!("subscription ended before TurnEnded for {expected_turn}");
    })
    .await
    .expect("turn ended within deadline")
}

struct BarrierHook {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    active: Arc<AtomicBool>,
}

impl Hook<BeforeTurn, Option<String>> for BarrierHook {
    fn call(
        &self,
        _input: BeforeTurn,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let entered = Arc::clone(&self.entered);
        let release = Arc::clone(&self.release);
        let active = self.active.load(Ordering::SeqCst);
        Box::pin(async move {
            if active {
                entered.notify_one();
                release.notified().await;
            }
            Ok(None)
        })
    }
}

struct BranchFixture {
    prompt: EntryId,
    assistant: EntryId,
    leaf: EntryId,
    text: &'static str,
    call: &'static str,
    result: &'static str,
    path: &'static str,
    added: u64,
    label: &'static str,
}

impl BranchFixture {
    fn records(&self, root: EntryId, at: Timestamp) -> TestResult<[Record; 3]> {
        Ok([
            Record::User(Entry {
                id: self.prompt,
                parent: Some(root),
                at,
                kind: EntryKind::User {
                    parts: vec![JournalPart::Text {
                        text: self.text.into(),
                    }],
                },
            }),
            Record::Assistant(Entry {
                id: self.assistant,
                parent: Some(self.prompt),
                at,
                kind: EntryKind::Assistant {
                    api: dal_core::Family::Chat,
                    model: "synth/test-model".into(),
                    route: None,
                    content: vec![dal_core::Block::ToolCall {
                        id: CallId::new(self.call),
                        name: "write_file".into(),
                        input: dal_core::RawJson::parse("{}")?,
                        replay: None,
                    }],
                    usage: dal_core::Usage {
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                        premium_requests: None,
                    },
                    stop: dal_core::AssistantStop::ToolUse,
                    replay: None,
                },
            }),
            Record::ToolResult(Entry {
                id: self.leaf,
                parent: Some(self.assistant),
                at,
                kind: EntryKind::ToolResult {
                    call: CallId::new(self.call),
                    name: "write_file".into(),
                    error: false,
                    parts: vec![JournalPart::Text {
                        text: self.result.into(),
                    }],
                    changes: vec![FileChange {
                        path: self.path.into(),
                        added: self.added,
                        removed: 0,
                    }],
                    elapsed_ms: None,
                },
            }),
        ])
    }
}

impl TestHarness {
    async fn branching_session(&self) -> TestResult<(SessionId, EntryId)> {
        let session = SessionId::new_v7();
        let mut journal = self.store().create_session(session);
        let at = Timestamp::now();
        let root = entry_id(1);
        let original = BranchFixture {
            prompt: entry_id(2),
            assistant: entry_id(3),
            leaf: entry_id(4),
            text: "Prompt A: initialize branch A",
            call: "call-a",
            result: "wrote A",
            path: "src/branch_a.rs",
            added: 10,
            label: "Branch A label",
        };
        let divergent = BranchFixture {
            prompt: entry_id(5),
            assistant: entry_id(6),
            leaf: entry_id(7),
            text: "Prompt B: initialize branch B",
            call: "call-b",
            result: "wrote B",
            path: "src/branch_b.rs",
            added: 5,
            label: "Branch B label",
        };
        let mut records = vec![Record::User(Entry {
            id: root,
            parent: None,
            at,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "Root prompt".into(),
                }],
            },
        })];
        // Each tool result answers its own branch's preceding assistant call.
        records.extend(original.records(root, at)?);
        records.push(Record::Leaf {
            at,
            to: Some(original.leaf),
        });
        let [prompt, assistant, result] = divergent.records(root, at)?;
        records.push(prompt);
        // Interleave the original label with the divergent branch's records.
        records.push(Record::Label {
            at,
            entry: original.leaf,
            label: Some(original.label.into()),
        });
        records.extend([assistant, result]);
        records.push(Record::Label {
            at,
            entry: divergent.leaf,
            label: Some(divergent.label.into()),
        });
        records.push(Record::Leaf {
            at,
            to: Some(divergent.leaf),
        });
        journal.append(records).await?;
        journal.close().await?;
        Ok((session, original.leaf))
    }
}

#[tokio::test]
async fn move_leaf_restores_path_preview_and_excludes_divergent_file_changes() -> TestResult {
    let ext = synthetic_extension(10, 5);
    let harness = start_harness(vec![ext]).await?;
    let (session_id, destination) = harness.branching_session().await?;
    let agent = harness.resume_agent(session_id, "branch-tester").await?;

    let view_b = agent.view(PageReq::default()).expect("view on branch B");
    assert!(
        view_b.session.preview.starts_with("Prompt B"),
        "initial active leaf preview must reflect Prompt B, got: {}",
        view_b.session.preview
    );
    assert!(
        view_b
            .changes
            .iter()
            .any(|c| c.path.as_ref() == "src/branch_b.rs"),
        "initial active view on Branch B must contain src/branch_b.rs, got: {:?}",
        view_b.changes
    );

    assert_eq!(
        view_b.tree.branches[0].label.as_deref(),
        Some("Branch B label")
    );

    let move_reply = agent
        .submit(Command::MoveLeaf(destination))
        .await
        .expect("move leaf to branch A");
    assert!(
        matches!(move_reply, Reply::Done(..)),
        "MoveLeaf must succeed"
    );

    let view_switched = agent
        .view(PageReq::default())
        .expect("view after MoveLeaf to branch A");

    assert_eq!(
        view_switched.tree.branches[0].label.as_deref(),
        Some("Branch A label")
    );
    assert!(
        view_switched.session.preview.starts_with("Prompt A"),
        "session preview after MoveLeaf back to Branch A must be Prompt A, but got: {}",
        view_switched.session.preview
    );
    assert!(
        view_switched.tree.branches[0]
            .preview
            .starts_with("Prompt A"),
        "branch outline preview after MoveLeaf must be Prompt A, but got: {}",
        view_switched.tree.branches[0].preview
    );
    assert!(
        view_switched
            .changes
            .iter()
            .any(|c| c.path.as_ref() == "src/branch_a.rs"),
        "view.changes must contain Branch A's file change (src/branch_a.rs), got: {:?}",
        view_switched.changes
    );
    assert!(
        !view_switched
            .changes
            .iter()
            .any(|c| c.path.as_ref() == "src/branch_b.rs"),
        "view.changes must NOT contain divergent Branch B's file change (src/branch_b.rs), got: {:?}",
        view_switched.changes
    );
    Ok(())
}

#[tokio::test]
async fn settling_turn_exposes_settling_state_and_does_not_satisfy_idle() -> TestResult {
    let barrier_entered = Arc::new(Notify::new());
    let barrier_release = Arc::new(Notify::new());
    let barrier_active = Arc::new(AtomicBool::new(false));

    let hook = BarrierHook {
        entered: Arc::clone(&barrier_entered),
        release: Arc::clone(&barrier_release),
        active: Arc::clone(&barrier_active),
    };

    let ext_gate = ExtensionBuilder::new("lifecycle-gate", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .on_before_turn(hook)
        .build()
        .expect("gate extension");

    let ext_model = synthetic_extension(10, 5);

    let harness = start_harness(vec![ext_gate, ext_model]).await?;
    let agent = harness.new_agent("lifecycle-tester").await?;
    let mut subscription = agent.subscribe(None).expect("subscription");

    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "First turn".into(),
            }],
        })
        .await
        .expect("submit prompt");
    let turn1 = match reply {
        Reply::Accepted { turn, .. } => turn,
        other => panic!("expected prompt Accepted, got: {other:?}"),
    };

    // Wait until turn1 has started so turn1's BeforeTurn hook has completed.
    wait_for_turn_start(&mut subscription, turn1).await;

    // Arm the barrier hook for the subsequent queued follow-up turn.
    barrier_active.store(true, Ordering::SeqCst);

    let follow_up_reply = agent
        .submit(Command::FollowUp {
            turn: turn1,
            content: vec![Part::Text {
                text: "Queued follow-up turn".into(),
            }],
        })
        .await
        .expect("submit follow-up");
    let queued_turn = match follow_up_reply {
        Reply::Queued { turn: Some(turn) } => turn,
        other => panic!("expected Reply::Queued with follow-up turn id, got: {other:?}"),
    };

    tokio::time::timeout(WAIT, barrier_entered.notified())
        .await
        .expect("follow-up BeforeTurn entered the barrier");

    let view = agent
        .view(PageReq::default())
        .expect("view during settling");
    assert_eq!(
        view.turn,
        TurnState::Settling { turn: turn1 },
        "turn state during settling window must be Settling {{ turn: {turn1} }}, not Idle!"
    );

    barrier_release.notify_one();

    wait_for_turn_end(&mut subscription, queued_turn).await;

    let final_view = agent.view(PageReq::default()).expect("final view");
    assert_eq!(final_view.turn, TurnState::Idle);
    Ok(())
}

#[test]
fn rejected_opening_can_release_its_owned_phase_without_turn_started() {
    let now = Timestamp::now();
    let (mut fold, _) = dal_core::Session::replay([], now).expect("empty fold");
    let mut effects = Vec::new();
    fold.step(
        dal_core::Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "opening".into(),
                }],
            },
            by: ClientId::new("rejected-opening"),
        },
        now,
        &mut effects,
    )
    .expect("reserve opening");
    let turn = match fold.phase() {
        dal_core::Phase::Opening { turn, .. } => *turn,
        other => panic!("expected opening, got {other:?}"),
    };
    let outcome = dal_core::HookOutcome::new(
        dal_core::HookEvent::Input,
        dal_core::HookVerdict::Input(dal_core::InputVerdict::Transform(Vec::new())),
    )
    .expect("input verdict");
    effects.clear();
    assert!(
        fold.step(
            dal_core::Event::Guard {
                turn,
                call: None,
                extension: None,
                outcome,
            },
            now,
            &mut effects,
        )
        .is_err()
    );
    fold.abandon_opening(turn);
    assert!(matches!(fold.phase(), dal_core::Phase::Idle));
    assert!(fold.leaf_entries().is_empty(), "no durable turn started");
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        dal_core::Effect::Emit(emit)
            if emit.updates.iter().any(|update| matches!(update, dal_core::UpdateKind::TurnStarted { .. }))
    )));
}

#[tokio::test]
async fn cancelled_opening_returns_idle_and_preserves_cancelled_stop() -> TestResult {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let active = Arc::new(AtomicBool::new(true));
    let gate = ExtensionBuilder::new("cancel-opening-gate", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .on_before_turn(BarrierHook {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            active,
        })
        .build()
        .expect("gate extension");
    let harness = start_harness(vec![gate, synthetic_extension(10, 5)]).await?;
    let agent = harness.new_agent("cancel-opening").await?;
    let mut subscription = agent.subscribe(None).expect("subscription");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "cancel before start".into(),
            }],
        })
        .await
        .expect("prompt");
    let turn = match reply {
        Reply::Accepted { turn, .. } => turn,
        other => panic!("expected accepted, got {other:?}"),
    };
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("hook entered");
    assert!(matches!(
        agent.view(PageReq::default()).expect("opening view").turn,
        TurnState::Settling { .. }
    ));
    agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await
        .expect("cancel opening");
    release.notify_one();
    assert_eq!(
        wait_for_turn_end(&mut subscription, turn).await,
        Stop::Cancelled
    );
    assert_eq!(
        agent.view(PageReq::default()).expect("cancelled view").turn,
        TurnState::Idle
    );
    Ok(())
}

#[tokio::test]
async fn reopened_session_restores_last_response_usage() -> TestResult {
    let ext = synthetic_extension(42, 18);
    let harness = start_harness(vec![ext]).await?;
    let agent = harness.new_agent("usage-tester").await?;
    let mut subscription = agent.subscribe(None).expect("subscription");

    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Run prompt with measured usage".into(),
            }],
        })
        .await
        .expect("submit prompt");
    let turn = match reply {
        Reply::Accepted { turn, .. } => turn,
        other => panic!("expected prompt Accepted, got: {other:?}"),
    };
    wait_for_turn_end(&mut subscription, turn).await;

    let view_before = agent.view(PageReq::default()).expect("view before close");
    assert_eq!(
        view_before.usage.usage.input_tokens, 42,
        "input tokens before close"
    );
    assert_eq!(
        view_before.usage.usage.output_tokens, 18,
        "output tokens before close"
    );

    let session_id = agent.session();
    harness.host.close(session_id).await.expect("close session");

    let resumed = harness.resume_agent(session_id, "resumed-client").await?;

    let view_after = resumed.view(PageReq::default()).expect("view after reopen");

    assert_eq!(
        view_after.usage.usage.input_tokens, 42,
        "reopened view must restore input_tokens (42), but got: {}",
        view_after.usage.usage.input_tokens
    );
    assert_eq!(
        view_after.usage.usage.output_tokens, 18,
        "reopened view must restore output_tokens (18), but got: {}",
        view_after.usage.usage.output_tokens
    );
    Ok(())
}
