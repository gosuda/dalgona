//! Adversarial lifetime proofs through the public `Host`/`Agent` API.
//!
//! Acknowledged barriers hold hooks or tools while the tests saturate service
//! queues, cancel turns, queue follow-ups, and shut down sessions.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test helpers fail loudly at their boundary")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::ext::{BeforeTurn, InputEvent, InputVerdict, SidecarOp};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, JournalPart, ModelId, ModelInfo, Name,
    Part, RawJson, ServiceSet, SidecarName, ThinkingLevel, ToolClass, ToolSpec, TurnId, TurnOp,
    TurnOpReply, Visibility, Workspace,
};
use dal_provider::{ProviderError, StreamEvent};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// A scripted response that reaches the extension tool and stays running.
const HELD_TOOL_REQUEST: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"opening-hold\",\"name\":\"held__boundary\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"opening-hold\",\"name\":\"held__boundary\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

/// One scripted provider request: a complete text stream ending the turn.
const REQUEST: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hel\"},{\"type\":\"text_delta\",\"text\":\"lo\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Concurrent safe-lane writes per opening phase: past the 64-bound opening
/// ingress, so at least one send must wait for the pump to drain it.
const BURST_WRITES: usize = 70;
/// Concurrent idle probes per opening phase, mixed into the same burst.
const BURST_POLLS: usize = 6;
/// Total burst service calls per phase; both phases burst, so a full
/// first turn completes `2 * BURST_CALLS`.
const BURST_CALLS: usize = BURST_WRITES + BURST_POLLS;
/// Burst service calls one saturated first turn must complete.
const EXPECTED_BURST_CALLS: usize = 2 * BURST_CALLS;

/// Bytes the burst hook writes after its flood; a later phase must read them
/// back, proving the acknowledged write persisted before acknowledgment.
const SENTINEL: &[u8] = b"opening-lifecycle-sentinel";

/// A host and its workspace, backed by a temporary data root.
struct LifecycleHost {
    host: Host,
    workspace: Workspace,
    tmp: tempfile::TempDir,
}

async fn configured_host(
    model: &str,
    script: Option<String>,
    extensions: Vec<dal_agent::ext::Extension>,
) -> LifecycleHost {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let config_text = if let Some(script) = script {
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, script).expect("fixture");
        format!(
            "model = \"{model}\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        )
    } else {
        format!("model = \"{model}\"\n")
    };
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    LifecycleHost {
        host,
        workspace,
        tmp,
    }
}

async fn scripted_host(
    requests: usize,
    extensions: Vec<dal_agent::ext::Extension>,
) -> LifecycleHost {
    configured_host(
        "openai/gpt-6-luna",
        Some(REQUEST.repeat(requests)),
        extensions,
    )
    .await
}

fn failed(message: String) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

/// A model-visible tool that holds the running turn at a real extension
/// boundary until cancellation or the test releases it.
struct HeldTool {
    name: Name,
    spec: Arc<ToolSpec>,
    entered: Arc<Notify>,
    gate: Arc<Notify>,
}

impl Tool for HeldTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        let cancel = cx.cancel().clone();
        Box::pin(async move {
            entered.notify_one();
            tokio::select! {
                () = gate.notified() => ToolOutcome::Ok(Box::new(ToolOutput::from_text("released"))),
                () = cancel.cancelled() => ToolOutcome::Interrupted,
            }
        })
    }
}

/// A public synthetic model whose entry count distinguishes provider opening
/// from the absence of a `TurnStarted` update.
struct EntryCounter {
    entries: Arc<AtomicUsize>,
}

impl ModelHandler for EntryCounter {
    fn run<'a>(
        &'a self,
        _request: dal_core::ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            let events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta {
                    text: "unexpected provider entry".into(),
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
                        premium_requests: None,
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

/// One member of a safe-lane flood issued from inside an opening hook.
enum BurstOp {
    Write { index: usize },
    Poll,
}

async fn run_burst_op(
    services: Arc<dyn dal_agent::ext::Services>,
    caller: dal_agent::ext::Caller,
    sidecar: SidecarName,
    phase: String,
    op: BurstOp,
    calls: Arc<AtomicUsize>,
) -> Result<(), HookError> {
    match op {
        BurstOp::Write { index } => {
            let bytes = format!("{phase}-burst-{index}").into_bytes();
            services
                .sidecar(
                    &caller,
                    SidecarOp::Write {
                        name: sidecar,
                        bytes,
                    },
                )
                .await
                .map_err(|error| failed(error.to_string()))?;
        }
        BurstOp::Poll => {
            let idle = services
                .turn(&caller, TurnOp::IsIdle)
                .await
                .map_err(|error| failed(error.to_string()))?;
            if !matches!(idle, TurnOpReply::Idle(false)) {
                return Err(failed("opening turn reported idle".to_string()));
            }
        }
    }
    calls.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

/// Floods the bounded opening-safe lane from both hook phases, then proves
/// an acknowledged sidecar write is visible to the next phase. Later turns
/// only read the sentinel back into `stash`: the burst arms fire once.
#[derive(Clone)]
struct BurstHook {
    sidecar: SidecarName,
    sentinel: SidecarName,
    sentinel_bytes: Vec<u8>,
    burst_input: Arc<AtomicBool>,
    burst_before: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    stash: Arc<std::sync::Mutex<Vec<Option<Vec<u8>>>>>,
}

impl BurstHook {
    async fn burst(
        services: Arc<dyn dal_agent::ext::Services>,
        caller: dal_agent::ext::Caller,
        sidecar: SidecarName,
        phase: &str,
        calls: Arc<AtomicUsize>,
    ) -> Result<(), HookError> {
        let writes = (0..BURST_WRITES).map(|index| {
            run_burst_op(
                Arc::clone(&services),
                caller.clone(),
                sidecar.clone(),
                phase.to_string(),
                BurstOp::Write { index },
                Arc::clone(&calls),
            )
        });
        let polls = (0..BURST_POLLS).map(|_| {
            run_burst_op(
                Arc::clone(&services),
                caller.clone(),
                sidecar.clone(),
                phase.to_string(),
                BurstOp::Poll,
                Arc::clone(&calls),
            )
        });
        for result in futures::future::join_all(writes.chain(polls)).await {
            result?;
        }
        Ok(())
    }

    async fn read_sentinel(
        services: Arc<dyn dal_agent::ext::Services>,
        caller: dal_agent::ext::Caller,
        sentinel: SidecarName,
    ) -> Result<Option<Vec<u8>>, HookError> {
        services
            .sidecar(&caller, SidecarOp::Read { name: sentinel })
            .await
            .map_err(|error| failed(error.to_string()))
    }
}

impl Hook<InputEvent, InputVerdict> for BurstHook {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let hook = self.clone();
        let services = Arc::clone(&cx.services);
        let caller = cx.caller.clone();
        Box::pin(async move {
            if hook.burst_input.swap(false, Ordering::SeqCst) {
                Self::burst(
                    Arc::clone(&services),
                    caller.clone(),
                    hook.sidecar.clone(),
                    "input",
                    Arc::clone(&hook.calls),
                )
                .await?;
                services
                    .sidecar(
                        &caller,
                        SidecarOp::Write {
                            name: hook.sentinel.clone(),
                            bytes: hook.sentinel_bytes.clone(),
                        },
                    )
                    .await
                    .map_err(|error| failed(error.to_string()))?;
                let back = Self::read_sentinel(services, caller, hook.sentinel.clone()).await?;
                if back.as_deref() != Some(hook.sentinel_bytes.as_slice()) {
                    return Err(failed(
                        "sentinel write was not visible after ack".to_string(),
                    ));
                }
            } else if let Ok(back) =
                Self::read_sentinel(services, caller, hook.sentinel.clone()).await
                && let Ok(mut stash) = hook.stash.lock()
            {
                stash.push(back);
            }
            Ok(InputVerdict::Continue)
        })
    }
}

impl Hook<BeforeTurn, Option<String>> for BurstHook {
    fn call(
        &self,
        _input: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let hook = self.clone();
        let services = Arc::clone(&cx.services);
        let caller = cx.caller.clone();
        Box::pin(async move {
            if hook.burst_before.swap(false, Ordering::SeqCst) {
                Self::burst(
                    Arc::clone(&services),
                    caller.clone(),
                    hook.sidecar.clone(),
                    "before_turn",
                    Arc::clone(&hook.calls),
                )
                .await?;
                let back = Self::read_sentinel(services, caller, hook.sentinel.clone()).await?;
                if back.as_deref() != Some(hook.sentinel_bytes.as_slice()) {
                    return Err(failed(
                        "sentinel write did not reach before_turn".to_string(),
                    ));
                }
            } else if let Ok(back) =
                Self::read_sentinel(services, caller, hook.sentinel.clone()).await
                && let Ok(mut stash) = hook.stash.lock()
            {
                stash.push(back);
            }
            Ok(None)
        })
    }
}

/// Holds one opening open until the test releases `gate`; the first hold
/// also raises `entered` so the test only acts while the hook is in flight.
/// The hold never spans two turns: `armed` fires once.
struct HeldHook {
    armed: Arc<AtomicBool>,
    entered: Arc<Notify>,
    gate: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
}

/// Records whether the held hook was dropped with its own turn token already
/// cancelled. A hook deadline also drops the future, but leaves this token
/// live, so the two release paths remain distinguishable without timing.
struct HeldHookDrop {
    dropped: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    cancel: CancellationToken,
}

impl Drop for HeldHookDrop {
    fn drop(&mut self) {
        self.cancelled
            .store(self.cancel.is_cancelled(), Ordering::SeqCst);
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl Hook<InputEvent, InputVerdict> for HeldHook {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let armed = Arc::clone(&self.armed);
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        let dropped = Arc::clone(&self.dropped);
        let cancelled = Arc::clone(&self.cancelled);
        let cancel = cx.cancel.clone();
        let hold = armed.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if hold {
                let _drop = HeldHookDrop {
                    dropped,
                    cancelled,
                    cancel,
                };
                entered.notify_one();
                gate.notified().await;
            }
            Ok(InputVerdict::Continue)
        })
    }
}

/// What one turn's subscription drain observed.
struct Drain {
    started: bool,
    stop: Option<dal_core::Stop>,
    hook_notices: Vec<String>,
}

/// Collects deliveries until this turn's `TurnEnded`, discounting every
/// other turn's traffic.
async fn drain_until_ended(subscription: &mut dal_agent::Subscription, turn: TurnId) -> Drain {
    let mut drain = Drain {
        started: false,
        stop: None,
        hook_notices: Vec::new(),
    };
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                dal_core::UpdateKind::TurnStarted { turn: seen, .. } if *seen == turn => {
                    drain.started = true;
                }
                dal_core::UpdateKind::TurnEnded { turn: seen, stop } if *seen == turn => {
                    drain.stop = Some(*stop);
                    break;
                }
                dal_core::UpdateKind::Notice(notice) if notice.turn == Some(turn) => {
                    let kind = notice.kind.to_string();
                    if kind.starts_with("hook.") {
                        drain.hook_notices.push(kind);
                    }
                }
                _ => {}
            }
        }
    })
    .await;
    outcome.expect("turn ends in time");
    drain
}

fn assert_accepted(reply: &dal_core::Reply, what: &str) -> TurnId {
    match reply {
        dal_core::Reply::Accepted { turn, .. } => *turn,
        other => panic!("{what} was not accepted: {other:?}"),
    }
}

fn prompt(text: &str) -> Command {
    Command::Prompt {
        expect: Expect::Idle,
        content: vec![Part::Text { text: text.into() }],
    }
}

/// The bounded opening-safe lane absorbs a Safe-service flood past its bound
/// from both hook phases: every call completes, the opening still reports
/// busy, no hook goes over budget, and the acknowledged sentinel write is
/// visible to the next phase and to a later turn.
#[tokio::test]
async fn opening_safe_lane_absorbs_a_burst_beyond_its_bound() {
    let calls = Arc::new(AtomicUsize::new(0));
    let stash = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook = BurstHook {
        sidecar: SidecarName::parse("burst.bin").expect("sidecar name"),
        sentinel: SidecarName::parse("sentinel.bin").expect("sidecar name"),
        sentinel_bytes: SENTINEL.to_vec(),
        burst_input: Arc::new(AtomicBool::new(true)),
        burst_before: Arc::new(AtomicBool::new(true)),
        calls: Arc::clone(&calls),
        stash: Arc::clone(&stash),
    };
    let services = ServiceSet::from_names(["sidecar", "turn"]).expect("service set");
    let extension = ExtensionBuilder::new("burst", "0.1.0", services)
        .expect("builder")
        .on_input(hook.clone())
        .on_before_turn(hook)
        .build()
        .expect("extension");
    let live = scripted_host(2, vec![extension]).await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let turn = assert_accepted(
        &agent.submit(prompt("burst")).await.expect("submit"),
        "first prompt",
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::EndTurn),
        "the saturated opening still runs its turn"
    );
    assert!(
        end.hook_notices.is_empty(),
        "no opening hook went over budget: {:?}",
        end.hook_notices
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        EXPECTED_BURST_CALLS,
        "every flooded safe call completed through the bounded lane"
    );
    let turn = assert_accepted(
        &agent.submit(prompt("read-back")).await.expect("submit"),
        "second prompt",
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert_eq!(end.stop, Some(dal_core::Stop::EndTurn));
    assert!(
        end.hook_notices.is_empty(),
        "read-back hooks stayed quiet: {:?}",
        end.hook_notices
    );
    let stash = stash.lock().expect("stash").clone();
    assert_eq!(
        stash,
        vec![Some(SENTINEL.to_vec()), Some(SENTINEL.to_vec())],
        "the acknowledged sentinel write survived to a later turn"
    );
}

/// Cancelling while the input hook holds ends the turn without journaling
/// model output, and the session stays usable: the next prompt runs on the
/// remaining fixture step, and the whole history survives a durable reopen.
#[expect(
    clippy::too_many_lines,
    reason = "cancel fixture walks prompt, cancel, and reopen in place"
)]
#[tokio::test]
async fn cancel_during_opening_ends_the_turn_without_model_output() {
    let entered = Arc::new(Notify::new());
    let hook_dropped = Arc::new(AtomicBool::new(false));
    let hook_cancelled = Arc::new(AtomicBool::new(false));
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .on_input(HeldHook {
            armed: Arc::new(AtomicBool::new(true)),
            entered: Arc::clone(&entered),
            gate: Arc::new(Notify::new()),
            dropped: Arc::clone(&hook_dropped),
            cancelled: Arc::clone(&hook_cancelled),
        })
        .build()
        .expect("extension");
    let live = scripted_host(2, vec![extension]).await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let before = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .entries
        .items
        .len();
    let turn = assert_accepted(
        &agent.submit(prompt("cancel-me")).await.expect("submit"),
        "first prompt",
    );
    entered.notified().await;
    let cancel = agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await;
    assert!(
        matches!(cancel, Ok(dal_core::Reply::Done(_))),
        "opening cancel resolves done, got {cancel:?}"
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert!(
        end.started,
        "the cancelled opening reaches TurnStarted before the retained token stops it"
    );
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::Cancelled),
        "cancellation while opening ends the turn cancelled"
    );
    assert!(
        hook_dropped.load(Ordering::SeqCst),
        "the held hook future must be dropped before the cancelled turn ends"
    );
    assert!(
        hook_cancelled.load(Ordering::SeqCst),
        "the held hook was dropped without its opening token; a deadline must not stand in for cancel"
    );
    let items = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .entries
        .items;
    let delta = &items[before..];
    assert!(
        delta.iter().any(|entry| matches!(
            &entry.kind,
            dal_core::EntryKind::User { parts }
            if parts.iter().any(|part| matches!(
                part,
                JournalPart::Text { text } if text.as_ref() == "cancel-me"
            ))
        )),
        "the cancelled prompt keeps its user entry"
    );
    assert!(
        !delta
            .iter()
            .any(|entry| matches!(&entry.kind, dal_core::EntryKind::Assistant { .. })),
        "the cancelled opening journals no model output"
    );
    let turn = assert_accepted(
        &agent.submit(prompt("after-cancel")).await.expect("submit"),
        "second prompt",
    );
    let end = drain_until_ended(&mut subscription, turn).await;
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::EndTurn),
        "the session stays usable after an opening cancel"
    );
    let saved = agent
        .view(dal_core::PageReq::default())
        .expect("saved view");
    live.host.close(id).await.expect("close");
    drop(agent);
    drop(subscription);
    let agent = live
        .host
        .open(
            SessionRef::Resume {
                key: id.to_string().into(),
                workspace: live.workspace,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("resume");
    let recovered = agent
        .view(dal_core::PageReq::default())
        .expect("replayed view");
    assert_eq!(
        recovered.entries.items, saved.entries.items,
        "cancel-then-reuse history survives a durable reopen"
    );
}

/// Classifies one update for the shutdown test: only this turn's start or
/// end matters, and either one disproves suppression.
fn opening_signal(update: &dal_core::Update, turn: TurnId) -> Option<&'static str> {
    match &update.kind {
        dal_core::UpdateKind::TurnStarted { turn: seen, .. } if *seen == turn => Some("started"),
        dal_core::UpdateKind::TurnEnded { turn: seen, .. } if *seen == turn => Some("ended"),
        _ => None,
    }
}

/// Shutting down while the input hook holds closes the session without
/// entering the public model handler. The entry counter is the provider-open
/// witness; update absence alone is not sufficient to prove suppression.
#[expect(
    clippy::too_many_lines,
    reason = "shutdown fixture walks control, hold, and drain in place"
)]
#[tokio::test]
async fn shutdown_during_opening_starts_no_provider_request() {
    let entered = Arc::new(Notify::new());
    let hook_dropped = Arc::new(AtomicBool::new(false));
    let hook_cancelled = Arc::new(AtomicBool::new(false));
    let entries = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .model(ModelRecord {
            id: ModelId::parse("fixture/entry").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(EntryCounter {
                entries: Arc::clone(&entries),
            }),
            export: None,
        })
        .on_input(HeldHook {
            armed: Arc::clone(&armed),
            entered: Arc::clone(&entered),
            gate: Arc::new(Notify::new()),
            dropped: Arc::clone(&hook_dropped),
            cancelled: Arc::clone(&hook_cancelled),
        })
        .build()
        .expect("extension");
    let live = configured_host("fixture/entry", None, vec![extension]).await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let control_turn = assert_accepted(
        &agent
            .submit(prompt("verify-model-route"))
            .await
            .expect("submit"),
        "control prompt",
    );
    let control_end = drain_until_ended(&mut subscription, control_turn).await;
    assert_eq!(control_end.stop, Some(dal_core::Stop::EndTurn));
    assert_eq!(
        entries.load(Ordering::SeqCst),
        1,
        "the model route is active"
    );
    armed.store(true, Ordering::SeqCst);
    let turn = assert_accepted(
        &agent.submit(prompt("shutdown-me")).await.expect("submit"),
        "first prompt",
    );
    entered.notified().await;
    let (report, kinds) = tokio::join!(live.host.shutdown(Duration::from_secs(10)), async {
        let mut kinds = Vec::new();
        let outcome = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let Some(delivery) = subscription.next().await else {
                    break;
                };
                let dal_agent::Delivery::Update(update) = delivery else {
                    continue;
                };
                if let Some(signal) = opening_signal(&update, turn) {
                    kinds.push(signal);
                }
            }
        })
        .await;
        outcome.expect("subscription closes after shutdown");
        kinds
    });
    assert!(
        hook_dropped.load(Ordering::SeqCst),
        "shutdown must drop the held hook future before returning"
    );
    assert!(
        hook_cancelled.load(Ordering::SeqCst),
        "shutdown must cancel the held opening token rather than wait for the hook deadline"
    );
    assert_eq!(
        report.sessions_closed, 1,
        "shutdown closed the held session"
    );
    assert_eq!(
        entries.load(Ordering::SeqCst),
        1,
        "shutdown must not enter the model handler again"
    );
    assert!(
        !kinds.contains(&"started"),
        "no TurnStarted reached subscribers: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"ended"),
        "a turn that never started never ends: {kinds:?}"
    );
}

/// A follow-up queued behind a genuinely running tool does not survive its
/// cancel as a turn: the fold discards it with its text in a notice, the queue
/// drains so `CancelQueued` rejects, and the same text runs cleanly as a fresh
/// prompt. The tool entry barrier proves the provider response crossed the
/// public extension boundary before the follow-up is submitted.
#[expect(
    clippy::too_many_lines,
    reason = "queued follow-up fixture walks run, cancel, and reuse in place"
)]
#[tokio::test]
async fn queued_follow_up_is_discarded_by_a_cancel_then_runs_fresh() {
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let tool_name = Name::parse("held__boundary").expect("tool name");
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .tool(
            Arc::new(HeldTool {
                name: tool_name.clone(),
                spec: Arc::new(ToolSpec {
                    name: tool_name,
                    description: "held running boundary".into(),
                    parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                    grammar: None,
                }),
                entered: Arc::clone(&entered),
                gate: Arc::clone(&gate),
            }),
            Visibility::Model,
        )
        .build()
        .expect("extension");
    let live = configured_host(
        "openai/gpt-6-luna",
        Some(format!("{HELD_TOOL_REQUEST}{REQUEST}{REQUEST}")),
        vec![extension],
    )
    .await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let first = assert_accepted(
        &agent.submit(prompt("first prompt")).await.expect("submit"),
        "first prompt",
    );
    let started = async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            if matches!(
                &update.kind,
                dal_core::UpdateKind::TurnStarted { turn: seen, .. } if *seen == first
            ) {
                break;
            }
        }
    };
    tokio::join!(
        entered.notified(),
        tokio::time::timeout(Duration::from_secs(30), started)
    )
    .1
    .expect("running turn starts");
    // The tool entry and TurnStarted precede this await. The held tool keeps
    // the running boundary open, so this reply cannot race a completed stream.
    let queued_turn = match agent
        .submit(Command::FollowUp {
            turn: first,
            content: vec![Part::Text {
                text: "second prompt".into(),
            }],
        })
        .await
        .expect("follow-up submit")
    {
        dal_core::Reply::Queued { turn: Some(next) } => next,
        other => panic!("follow-up was not queued: {other:?}"),
    };
    let cancel = agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(first),
        })
        .await;
    assert!(
        matches!(cancel, Ok(dal_core::Reply::Done(_))),
        "cancel resolves done, got {cancel:?}"
    );
    gate.notify_one();
    let mut stop = None;
    let mut discarded = Vec::new();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let delivery = subscription.next().await.expect("stream open");
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                dal_core::UpdateKind::TurnEnded {
                    turn: seen,
                    stop: ended,
                } if *seen == first => {
                    stop = Some(*ended);
                    break;
                }
                dal_core::UpdateKind::Notice(notice)
                    if notice.turn == Some(first) && notice.kind.as_ref() == "discarded" =>
                {
                    discarded.push(notice.text.to_string());
                }
                _ => {}
            }
        }
    })
    .await;
    outcome.expect("cancelled turn ends in time");
    assert_eq!(
        stop,
        Some(dal_core::Stop::Cancelled),
        "the running first turn ends cancelled"
    );
    assert_eq!(
        discarded,
        vec![String::from("second prompt")],
        "the queued follow-up is discarded with its exact text"
    );
    let gone = agent
        .submit(Command::CancelQueued { turn: queued_turn })
        .await;
    let Err(gone) = gone else {
        panic!("the discarded follow-up must leave no queue entry");
    };
    assert!(
        matches!(gone, dal_agent::AgentError::Invalid(_)),
        "a missing queue entry is a validation refusal: {gone:?}"
    );
    assert_eq!(
        gone.to_string(),
        format!(
            "turn {queued_turn} has no queued follow-up. If the follow-up already started, cancel its turn instead."
        ),
        "the refusal names the turn with no queue entry"
    );
    let second = assert_accepted(
        &agent.submit(prompt("second prompt")).await.expect("submit"),
        "fresh prompt",
    );
    let end = drain_until_ended(&mut subscription, second).await;
    assert_eq!(
        end.stop,
        Some(dal_core::Stop::EndTurn),
        "the discarded text runs cleanly as a fresh prompt"
    );
    let items = agent
        .view(dal_core::PageReq::default())
        .expect("view")
        .entries
        .items;
    let users: Vec<String> = items
        .iter()
        .filter_map(|entry| match &entry.kind {
            dal_core::EntryKind::User { parts } => Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        JournalPart::Text { text } => Some(text.to_string()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(
        users,
        vec![String::from("first prompt"), String::from("second prompt")],
        "the journal holds both prompts exactly once"
    );
}

struct HeldSession {
    live: LifecycleHost,
    agent: dal_agent::Agent,
    subscription: dal_agent::Subscription,
    turn: TurnId,
    gate: Arc<Notify>,
}

async fn held_session() -> HeldSession {
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let name = Name::parse("held__boundary").expect("tool name");
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .tool(
            Arc::new(HeldTool {
                name: name.clone(),
                spec: Arc::new(ToolSpec {
                    name,
                    description: "held running boundary".into(),
                    parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                    grammar: None,
                }),
                entered: Arc::clone(&entered),
                gate: Arc::clone(&gate),
            }),
            Visibility::Model,
        )
        .build()
        .expect("extension");
    let live = configured_host(
        "openai/gpt-6-luna",
        Some(format!("{HELD_TOOL_REQUEST}{REQUEST}{REQUEST}{REQUEST}")),
        vec![extension],
    )
    .await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let subscription = agent.subscribe(None).expect("subscribe");
    let turn = assert_accepted(
        &agent.submit(prompt("first prompt")).await.expect("submit"),
        "first prompt",
    );
    tokio::time::timeout(Duration::from_secs(30), entered.notified())
        .await
        .expect("tool entered");
    HeldSession {
        live,
        agent,
        subscription,
        turn,
        gate,
    }
}

async fn queue_follow_up(agent: &dal_agent::Agent, turn: TurnId, text: &str) -> TurnId {
    match agent
        .submit(Command::FollowUp {
            turn,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("queue follow-up")
    {
        dal_core::Reply::Queued { turn: Some(turn) } => turn,
        other => panic!("follow-up was not queued: {other:?}"),
    }
}

#[tokio::test]
async fn cancelling_a_non_last_reservation_keeps_the_later_turn_runnable() {
    let mut held = held_session().await;
    let removed = queue_follow_up(&held.agent, held.turn, "remove me").await;
    let retained = queue_follow_up(&held.agent, held.turn, "keep me").await;
    held.agent
        .submit(Command::CancelQueued { turn: removed })
        .await
        .expect("cancel first queued turn");
    held.gate.notify_one();
    assert_eq!(
        drain_until_ended(&mut held.subscription, held.turn)
            .await
            .stop,
        Some(dal_core::Stop::EndTurn)
    );
    assert_eq!(
        drain_until_ended(&mut held.subscription, retained)
            .await
            .stop,
        Some(dal_core::Stop::EndTurn)
    );
    let view = held.agent.view(dal_core::PageReq::default()).expect("view");
    let texts: Vec<_> = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            dal_core::EntryKind::User { parts } => Some(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        JournalPart::Text { text } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["first prompt", "keep me"]);
    held.live
        .host
        .close(held.agent.session())
        .await
        .expect("close");
}

#[tokio::test]
async fn discarded_reservations_are_not_reused_after_reopen() {
    let mut held = held_session().await;
    let discarded = queue_follow_up(&held.agent, held.turn, "discard me").await;
    held.agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(held.turn),
        })
        .await
        .expect("cancel");
    held.gate.notify_one();
    assert_eq!(
        drain_until_ended(&mut held.subscription, held.turn)
            .await
            .stop,
        Some(dal_core::Stop::Cancelled)
    );
    let session = held.agent.session();
    held.live.host.close(session).await.expect("close");
    drop(held.agent);
    drop(held.subscription);
    let agent = held
        .live
        .host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: held.live.workspace,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("reopen");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let fresh = assert_accepted(
        &agent
            .submit(prompt("fresh after restart"))
            .await
            .expect("prompt"),
        "reopened prompt",
    );
    assert!(
        fresh > discarded,
        "an acknowledged identity must never be reassigned"
    );
    let Err(reissued) = agent
        .submit(Command::CancelQueued { turn: discarded })
        .await
    else {
        panic!("a discarded reservation must not be cancellable after reopen");
    };
    assert_eq!(
        reissued.to_string(),
        format!(
            "turn {discarded} has no queued follow-up. If the follow-up already started, cancel its turn instead."
        ),
        "the reopened session holds no entry for the discarded turn"
    );
    assert_eq!(
        drain_until_ended(&mut subscription, fresh).await.stop,
        Some(dal_core::Stop::EndTurn)
    );
    held.live.host.close(session).await.expect("close reopened");
}

/// A failed journal sync refuses the follow-up as a validation error that
/// names the failed write, not as a wrong-turn or closed-session refusal.
fn assert_sync_refusal(reply: &Result<dal_core::Reply, dal_agent::AgentError>, what: &str) {
    let Err(error @ dal_agent::AgentError::Invalid(_)) = reply else {
        panic!("{what}: expected a validation refusal, got {reply:?}");
    };
    let text = error.to_string();
    assert!(
        text.starts_with("journal write failed:")
            && text.contains("injected sync failure")
            && text.contains("removed the partial record"),
        "{what}: the refusal names the failed journal write: {text}"
    );
}

#[tokio::test]
async fn failed_reservation_sync_never_acknowledges_a_queued_turn() {
    let held = held_session().await;
    let session = held.agent.session();
    let store = dal_store::Store::new(
        held.live.tmp.path().join("data"),
        held.live.workspace.clone(),
        dal_core::Product::Dal,
    );
    store.arm_append_sync_fault(session);
    let reply = held
        .agent
        .submit(Command::FollowUp {
            turn: held.turn,
            content: vec![Part::Text {
                text: "must not be queued".into(),
            }],
        })
        .await;
    assert_sync_refusal(&reply, "a failed sync cannot return a queued identity");
    let again = held
        .agent
        .submit(Command::FollowUp {
            turn: held.turn,
            content: vec![Part::Text {
                text: "must fail closed".into(),
            }],
        })
        .await;
    assert_sync_refusal(&again, "the broken session keeps failing closed");
    held.live
        .host
        .close(session)
        .await
        .expect("close broken session");
    drop(held.agent);
    drop(held.subscription);
    let (journal, _) = store
        .open_session(session)
        .await
        .expect("reopen durable prefix");
    assert!(!journal.records().iter().any(|record| matches!(record,
        dal_core::Record::TurnReserved { turn, .. } if *turn > held.turn)));
}

#[tokio::test]
async fn an_unstarted_first_prompt_retains_its_identity_after_reopen() {
    let entered = Arc::new(Notify::new());
    let extension = ExtensionBuilder::new("held", "0.1.0", ServiceSet::default())
        .expect("builder")
        .on_input(HeldHook {
            armed: Arc::new(AtomicBool::new(true)),
            entered: Arc::clone(&entered),
            gate: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
        .build()
        .expect("extension");
    let live = scripted_host(1, vec![extension]).await;
    let agent = live
        .host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("open");
    let first = assert_accepted(
        &agent
            .submit(prompt("unstarted prompt"))
            .await
            .expect("prompt"),
        "opening prompt",
    );
    tokio::time::timeout(Duration::from_secs(30), entered.notified())
        .await
        .expect("input hook entered");
    let session = agent.session();
    live.host
        .close(session)
        .await
        .expect("close during opening");
    drop(agent);
    let agent = live
        .host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: live.workspace,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("resume reservation-only history");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let fresh = assert_accepted(
        &agent.submit(prompt("after restart")).await.expect("prompt"),
        "fresh prompt",
    );
    assert!(
        fresh > first,
        "opening acknowledgment must survive before any user entry"
    );
    assert_eq!(
        drain_until_ended(&mut subscription, fresh).await.stop,
        Some(dal_core::Stop::EndTurn)
    );
    live.host.close(session).await.expect("close reopened");
}
