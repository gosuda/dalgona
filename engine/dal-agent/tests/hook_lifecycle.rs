//! Native hooks round-trip through the real Host without redispatching their opening.
#![expect(
    clippy::expect_used,
    reason = "fixture setup must fail at its boundary"
)]

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::BeforeTurn;
use dal_core::{
    AgentsOp, AgentsReply, Answer, Caps, ClientId, Command, Config, ConfigProduct, ContextItem,
    Expect, InputEvent, InputVerdict, MailMode, ModelId, ModelRequest, ModelRoute, Origin, Part,
    Purpose, Question, RawJson, Receipt, RequestParams, ServiceSet, ThinkingLevel, TurnOp,
    TurnOpReply, UpdateKind, Workspace,
};
use dal_provider::{ProviderError, StreamEvent};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(20);

#[expect(
    clippy::needless_pass_by_value,
    reason = "test helper takes ownership to keep map_err(failed) readable"
)]
fn failed(message: impl ToString) -> HookError {
    HookError::Failed {
        message: message.to_string().into(),
    }
}

fn require(condition: bool, message: &str) -> Result<(), HookError> {
    if condition {
        Ok(())
    } else {
        Err(failed(message))
    }
}

async fn round_trip(cx: HookCx) -> Result<(), HookError> {
    round_trip_mail(&cx).await?;
    round_trip_turn(&cx).await?;
    round_trip_infer_ask(&cx).await
}

async fn round_trip_mail(cx: &HookCx) -> Result<(), HookError> {
    let services = &cx.services;
    let caller = &cx.caller;
    let receipt = services
        .agents(
            caller,
            AgentsOp::Send {
                to: cx.session,
                text: "hook-steer".into(),
                mode: MailMode::Steer,
                reply_to: None,
            },
        )
        .await
        .map_err(failed)?;
    require(
        matches!(receipt, AgentsReply::Delivered(Receipt::Buffered)),
        "opening steer must be buffered",
    )?;
    let mail = services
        .agents(
            caller,
            AgentsOp::Recv {
                after: None,
                timeout: None,
            },
        )
        .await
        .map_err(failed)?;
    require(
        matches!(mail, AgentsReply::Received { mail, .. } if mail.iter().any(|item| item.text.as_ref() == "hook-steer")),
        "acknowledged mail must be readable",
    )?;
    Ok(())
}

async fn round_trip_turn(cx: &HookCx) -> Result<(), HookError> {
    let services = &cx.services;
    let caller = &cx.caller;
    let idle = services
        .turn(caller, TurnOp::IsIdle)
        .await
        .map_err(failed)?;
    require(
        idle == TurnOpReply::Idle(false),
        "hook opening must remain busy",
    )?;
    let cancel = services
        .turn(caller, TurnOp::Cancel)
        .await
        .map_err(failed)?;
    require(
        cancel == TurnOpReply::Idle(true),
        "cancel keeps its running-slot semantics",
    )?;
    let steer = services
        .turn(
            caller,
            TurnOp::Steer {
                text: "unused-steer".into(),
            },
        )
        .await
        .map_err(failed)?;
    require(
        steer == TurnOpReply::Idle(true),
        "steer keeps its running-slot semantics",
    )?;
    Ok(())
}

async fn round_trip_infer_ask(cx: &HookCx) -> Result<(), HookError> {
    let services = &cx.services;
    let caller = &cx.caller;
    // More usage callbacks than the command-channel bound: leaving Inferred
    // behind this opening would fill the mailbox and strand inner inference.
    services
        .infer(
            caller,
            ModelRequest {
                purpose: Purpose::Turn,
                model: ModelRoute::synthetic("fixture/accounting").map_err(failed)?,
                system: "".into(),
                tools: Arc::from(Vec::new()),
                context: Arc::from(vec![ContextItem::User {
                    parts: vec![Part::Text {
                        text: "inner".into(),
                    }],
                }]),
                params: RequestParams::default(),
                cache_key: None,
            },
        )
        .await
        .map_err(failed)?;
    let answer = services
        .ask(
            caller,
            Question::Text {
                prompt: "hook-question".into(),
                placeholder: None,
            },
        )
        .await
        .map_err(failed)?;
    require(
        answer
            == Some(Answer::Value(
                RawJson::parse("\"answered\"").map_err(failed)?,
            )),
        "hook question must resolve through the controller",
    )?;
    let wake = services
        .turn(
            caller,
            TurnOp::Wake {
                text: "after".into(),
                sources: vec!["hook".into()],
                job_ids: Vec::new(),
            },
        )
        .await
        .map_err(failed)?;
    require(
        wake == TurnOpReply::Woken,
        "wake must queue through the existing fold",
    )?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookPhase {
    Input,
    BeforeTurn,
}

struct NativeHook {
    phase: HookPhase,
    completed: Arc<AtomicUsize>,
    input_entries: Arc<AtomicUsize>,
    bootstrap: bool,
}

impl Hook<InputEvent, InputVerdict> for NativeHook {
    fn call(
        &self,
        event: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let is_target = event
            .content
            .iter()
            .any(|part| matches!(part, Part::Text { text } if text.as_ref() == "target"));
        let target = self.phase == HookPhase::Input && is_target;
        let completed = Arc::clone(&self.completed);
        let input_entries = Arc::clone(&self.input_entries);
        Box::pin(async move {
            if is_target {
                input_entries.fetch_add(1, Ordering::SeqCst);
            }
            if target {
                round_trip(cx).await?;
                completed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(InputVerdict::Continue)
        })
    }
}

impl Hook<BeforeTurn, Option<String>> for NativeHook {
    fn call(
        &self,
        event: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let target = self.phase == HookPhase::BeforeTurn && event.text.as_ref() == "target";
        let bootstrap = self.bootstrap && event.text.as_ref() == "bootstrap";
        let completed = Arc::clone(&self.completed);
        Box::pin(async move {
            if bootstrap {
                let wake = cx
                    .services
                    .turn(
                        &cx.caller,
                        TurnOp::Wake {
                            text: "target".into(),
                            sources: vec!["bootstrap".into()],
                            job_ids: Vec::new(),
                        },
                    )
                    .await
                    .map_err(failed)?;
                require(wake == TurnOpReply::Woken, "bootstrap wake must queue")?;
            }
            if target {
                round_trip(cx).await?;
                completed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(None)
        })
    }
}

/// The first model entry is a causal barrier for queuing a real follow-up.
struct Model {
    hold_first: bool,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    entries: Arc<AtomicUsize>,
}

impl ModelHandler for Model {
    fn run<'a>(
        &'a self,
        _request: dal_core::ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let hold = self.entries.fetch_add(1, Ordering::SeqCst) == 0 && self.hold_first;
        Box::pin(async move {
            if hold {
                self.entered.notify_one();
                self.release.notified().await;
            }
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "done".into(),
                }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
                Ok(StreamEvent::Usage {
                    usage: dal_core::Usage {
                        input_tokens: 1,
                        cached_input_tokens: 0,
                        output_tokens: 1,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                    },
                }),
                Ok(StreamEvent::Stop {
                    reason: dal_provider::StopReason::EndTurn,
                }),
            ];
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

struct AccountingModel;

impl ModelHandler for AccountingModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async {
            let mut events: Vec<Result<StreamEvent, ProviderError>> = (0..70)
                .map(|_| {
                    Ok(StreamEvent::Usage {
                        usage: dal_core::Usage {
                            input_tokens: 1,
                            cached_input_tokens: 0,
                            output_tokens: 1,
                            reasoning_tokens: None,
                            cache_write_tokens: 0,
                            cost_usd: None,
                        },
                    })
                })
                .collect();
            events.push(Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }));
            events.push(Ok(StreamEvent::Stop {
                reason: dal_provider::StopReason::EndTurn,
            }));
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Prompt,
    FollowUp,
    Wake,
}

fn prompt(text: &str) -> Command {
    Command::Prompt {
        content: vec![Part::Text { text: text.into() }],
        expect: Expect::Idle,
    }
}

async fn drain(agent: &Agent, subscription: &mut Subscription, turns: usize, origin: Origin) {
    let mut ended = 0;
    let mut asks = 0;
    let mut grants = 0;
    let mut starts = Vec::new();
    tokio::time::timeout(WAIT, async {
        while ended < turns {
            let delivery = subscription.next().await.expect("session remains open");
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    let answer = match &request.question {
                        Question::Grant { .. } => {
                            grants += 1;
                            Answer::ApproveForSession
                        }
                        Question::Text { prompt, .. } if prompt.as_ref() == "hook-question" => {
                            asks += 1;
                            Answer::Value(RawJson::parse("\"answered\"").expect("answer JSON"))
                        }
                        other => return Err(format!("unexpected request: {other:?}")),
                    };
                    agent
                        .answer(request.id, answer)
                        .await
                        .expect("authorized controller answer");
                }
                UpdateKind::TurnStarted { turn, .. } => {
                    if starts.contains(turn) {
                        return Err(format!("duplicate opening for {turn:?}"));
                    }
                    starts.push(*turn);
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    if *stop != dal_core::Stop::EndTurn {
                        return Err(format!("unexpected stop: {stop:?}"));
                    }
                    ended += 1;
                }
                UpdateKind::Notice(notice) if notice.kind.as_ref().starts_with("hook.") => {
                    return Err(format!("hook failed: {}", notice.text));
                }
                _ => {}
            }
        }
        Ok(())
    })
    .await
    .expect("hooks and turns finish before their deadlines")
    .expect("clean lifecycle");
    assert_eq!(starts.len(), turns, "exactly one start per provider turn");
    assert_eq!(asks, 1, "the target hook asked exactly once");
    assert_eq!(
        grants,
        usize::from(origin == Origin::User),
        "existing grants remain in force"
    );
}

struct ExerciseProbes {
    completed: Arc<AtomicUsize>,
    input_entries: Arc<AtomicUsize>,
    entries: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ExerciseProbes {
    fn fresh() -> Self {
        Self {
            completed: Arc::new(AtomicUsize::new(0)),
            input_entries: Arc::new(AtomicUsize::new(0)),
            entries: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

async fn start_exercise_host(
    source: Source,
    phase: HookPhase,
    origin: Origin,
    data: std::path::PathBuf,
    workspace_dir: &std::path::Path,
    probes: &ExerciseProbes,
) -> Host {
    let extension = ExtensionBuilder::new(
        "native-hooks",
        "0.1.0",
        ServiceSet::from_names(["agents", "turn", "ask"]).expect("services"),
    )
    .expect("builder")
    .with_origin(origin, None)
    .on_input(NativeHook {
        phase,
        completed: Arc::clone(&probes.completed),
        input_entries: Arc::clone(&probes.input_entries),
        bootstrap: false,
    })
    .on_before_turn(NativeHook {
        phase,
        completed: Arc::clone(&probes.completed),
        input_entries: Arc::clone(&probes.input_entries),
        bootstrap: source == Source::Wake,
    })
    .build()
    .expect("native hooks");
    let model = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())
        .expect("model builder")
        .model(ModelRecord {
            id: ModelId::parse("fixture/end").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Model {
                hold_first: source == Source::FollowUp,
                entered: Arc::clone(&probes.entered),
                release: Arc::clone(&probes.release),
                entries: Arc::clone(&probes.entries),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: ModelId::parse("fixture/accounting").expect("accounting model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(AccountingModel),
            export: None,
        })
        .build()
        .expect("model extension");
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )
    .expect("config");
    Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![extension, model],
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::new(),
            cwd: workspace_dir.to_path_buf(),
            sandbox_helper: None,
        },
    )
    .await
    .expect("host")
}

async fn drive_exercise_seed(agent: &Agent, source: Source, probes: &ExerciseProbes) -> usize {
    let text = match source {
        Source::Prompt => "target",
        Source::FollowUp => "seed",
        Source::Wake => "bootstrap",
    };
    let first = agent.submit(prompt(text)).await.expect("submit");
    let first = match first {
        dal_core::Reply::Accepted { turn, .. } => Some(turn),
        _ => None,
    }
    .expect("prompt must be accepted");
    if source == Source::FollowUp {
        tokio::time::timeout(WAIT, probes.entered.notified())
            .await
            .expect("first model entry");
        let reply = agent
            .submit(Command::FollowUp {
                turn: first,
                content: vec![Part::Text {
                    text: "target".into(),
                }],
            })
            .await
            .expect("follow-up");
        assert!(
            matches!(reply, dal_core::Reply::Queued { turn: Some(_), .. }),
            "follow-up must really queue: {reply:?}"
        );
        probes.release.notify_one();
    }
    if source == Source::Prompt { 2 } else { 3 }
}

async fn check_exercise_reopen(host: &Host, agent: &Agent) {
    let persisted = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .entries
        .items;
    let session = agent.session();
    let workspace = agent
        .view(dal_core::PageReq::default())
        .expect("workspace view")
        .session
        .workspace;
    host.close(session).await.expect("close");
    let resumed = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace,
            },
            ClientId::new("controller"),
        )
        .await
        .expect("durable reopen");
    assert_eq!(
        resumed
            .view(dal_core::PageReq::default())
            .expect("reopened view")
            .entries
            .items,
        persisted,
        "wake and turn history survive reopen"
    );
    host.close(session).await.expect("close resumed session");
}

async fn exercise(source: Source, phase: HookPhase, origin: Origin) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data");
    std::fs::create_dir_all(&workspace_dir).expect("workspace");
    let probes = ExerciseProbes::fresh();
    let host = start_exercise_host(source, phase, origin, data, &workspace_dir, &probes).await;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
                name: None,
            },
            ClientId::new("controller"),
        )
        .await
        .expect("session");
    let mut subscription = agent.subscribe(None).expect("answering subscription");
    let turns = drive_exercise_seed(&agent, source, &probes).await;
    drain(&agent, &mut subscription, turns, origin).await;
    assert_eq!(
        probes.completed.load(Ordering::SeqCst),
        1,
        "target hook completes once"
    );
    assert_eq!(
        probes.input_entries.load(Ordering::SeqCst),
        usize::from(source != Source::Wake),
        "wake skips input; prompts and follow-ups run it exactly once"
    );
    assert_eq!(
        probes.entries.load(Ordering::SeqCst),
        turns,
        "no recursive or cancelled provider openings"
    );
    check_exercise_reopen(&host, &agent).await;
}

#[tokio::test]
async fn prompt_input_native_services_preserve_grants_and_finish() {
    exercise(Source::Prompt, HookPhase::Input, Origin::User).await;
}

#[tokio::test]
async fn prompt_before_turn_native_services_do_not_redispatch_opening() {
    exercise(Source::Prompt, HookPhase::BeforeTurn, Origin::Builtin).await;
}

#[tokio::test]
async fn follow_up_input_native_services_finish_while_settling() {
    exercise(Source::FollowUp, HookPhase::Input, Origin::Builtin).await;
}

#[tokio::test]
async fn follow_up_before_turn_native_services_preserve_the_next_turn() {
    exercise(Source::FollowUp, HookPhase::BeforeTurn, Origin::Builtin).await;
}

#[tokio::test]
async fn wake_before_turn_native_services_do_not_run_input_hooks() {
    exercise(Source::Wake, HookPhase::BeforeTurn, Origin::Builtin).await;
}
