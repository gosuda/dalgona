//! Actor boundary for judged stream watchers: the provider stream is
//! physically destroyed before the judged wait proceeds, and cancellation
//! delivers no late fire.

#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, HookError, ModelCx, ModelError,
    ModelHandler, ModelRecord, StreamWatch, StreamWatchRecord, TurnInfo, WatchFactory,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::ext::{Channel, StreamFire, StreamFireAction, StreamVerdict};
use dal_core::{
    Block, CancelScope, Caps, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, ModelId,
    ModelRequest, PageReq, Part, Reply, ServiceSet, Stop, ThinkingLevel, UpdateKind, View,
    Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

const MODEL: &str = "kit/drop-check";
const RULE: &str = "judge-wire";

/// Shared proof state: the interrupting stream announces its own
/// destruction here, and the judged wait refuses to proceed until it does.
#[derive(Clone, Default)]
struct ReleaseWire {
    released: Arc<AtomicBool>,
    release: Arc<Notify>,
}

impl ReleaseWire {
    fn flag(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    /// Waits for the destruction signal; cancellation wins the race.
    ///
    /// The waiter registers through `enable()` before the flag check, so a
    /// `notify_waiters` between the check and the wait cannot be lost.
    async fn await_release(&self, cancel: &CancellationToken) -> Result<(), HookError> {
        let notified = self.release.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.flag() {
            return Ok(());
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(HookError::Cancelled),
            () = &mut notified => Ok(()),
        }
    }
}

/// Handoff between the test and the watcher's judged wait.
///
/// The watcher signals entry through `entered` and then blocks on `verdict`,
/// so the test drives the judged wait with explicit synchronization instead
/// of sleep ordering.
#[derive(Clone)]
struct JudgedGate {
    entered: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    verdict: Arc<Mutex<Option<oneshot::Receiver<bool>>>>,
}

impl JudgedGate {
    fn armed() -> (Self, oneshot::Receiver<()>, oneshot::Sender<bool>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (verdict_tx, verdict_rx) = oneshot::channel();
        (
            Self {
                entered: Arc::new(Mutex::new(Some(entered_tx))),
                verdict: Arc::new(Mutex::new(Some(verdict_rx))),
            },
            entered_rx,
            verdict_tx,
        )
    }
}

/// The session's native model: its first stream interrupts on `TRIGGER` and
/// announces its own destruction; its second stream answers cleanly.
struct DropProbe {
    opened: AtomicUsize,
    wire: ReleaseWire,
}

impl DropProbe {
    fn new(wire: ReleaseWire) -> Self {
        Self {
            opened: AtomicUsize::new(0),
            wire,
        }
    }

    fn stream(&self, attempt: usize) -> EventStream {
        let usage = dal_core::Usage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        };
        let events = if attempt == 0 {
            vec![
                StreamEvent::TextDelta {
                    text: "partial attempt TRIGGER".into(),
                },
                StreamEvent::TextDelta {
                    text: " tail after the interrupt".into(),
                },
                StreamEvent::ToolCallsDone { calls: Vec::new() },
                StreamEvent::Usage { usage },
                StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        } else {
            vec![
                StreamEvent::TextDelta {
                    text: "clean answer after judgment".into(),
                },
                StreamEvent::ToolCallsDone { calls: Vec::new() },
                StreamEvent::Usage { usage },
                StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ]
        };
        let items: Vec<Result<StreamEvent, ProviderError>> = events.into_iter().map(Ok).collect();
        let interrupted = attempt == 0;
        let wire = self.wire.clone();
        EventStream::new(futures::stream::iter(items), move || {
            if interrupted {
                wire.released.store(true, Ordering::SeqCst);
                wire.release.notify_waiters();
            }
        })
    }
}

impl ModelHandler for DropProbe {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let attempt = self.opened.fetch_add(1, Ordering::SeqCst);
        let stream = self.stream(attempt);
        Box::pin(std::future::ready(Ok(stream)))
    }
}

fn drop_model_extension(gate: JudgedGate, wire: ReleaseWire, probe: Arc<DropProbe>) -> Extension {
    let record = ModelRecord {
        id: ModelId::parse(MODEL).expect("model id"),
        caps: Caps {
            context_window: Some(100_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: probe,
        export: None,
    };
    ExtensionBuilder::new("judge-boundary", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .output_stream(Arc::new(JudgedWatchFactory { gate, wire }))
        .model(record)
        .build()
        .expect("watcher extension")
}

struct JudgedWatchFactory {
    gate: JudgedGate,
    wire: ReleaseWire,
}

impl WatchFactory for JudgedWatchFactory {
    fn start(&self, _turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        Some(Box::new(JudgedWatcher {
            gate: self.gate.clone(),
            wire: self.wire.clone(),
            fired: false,
        }))
    }
}

/// A native judged watcher. Its interrupt fire carries `judged: true`, so
/// the driver must destroy the provider stream before the judged wait makes
/// progress; the watcher enforces that by waiting on the stream's own drop
/// notification before it even signals entry.
struct JudgedWatcher {
    gate: JudgedGate,
    wire: ReleaseWire,
    fired: bool,
}

impl StreamWatch for JudgedWatcher {
    fn feed(&mut self, _channel: Channel, delta: &str) -> StreamVerdict {
        if delta.contains("TRIGGER") && !self.fired {
            self.fired = true;
            StreamVerdict::Interrupt {
                rule: RULE.into(),
                inject: "Settle the judged fire.".into(),
            }
        } else {
            StreamVerdict::Continue
        }
    }

    fn finish(&mut self) -> StreamVerdict {
        StreamVerdict::Continue
    }

    fn take_fires(&mut self) -> Vec<StreamFire> {
        if !self.fired {
            return Vec::new();
        }
        self.fired = false;
        vec![StreamFire {
            index: 0,
            rule: RULE.into(),
            action: StreamFireAction::Interrupt,
            text: Some("Settle the judged fire.".into()),
            judged: true,
        }]
    }

    fn resolve_judged(
        &mut self,
        _fire: usize,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<bool, HookError>> {
        let gate = self.gate.clone();
        let wire = self.wire.clone();
        Box::pin(async move {
            wire.await_release(&cancel).await?;
            let entered = lock(&gate.entered).take();
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            let Some(verdict) = lock(&gate.verdict).take() else {
                return Ok(false);
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(HookError::Cancelled),
                verdict = verdict => verdict.map_err(|_| HookError::Cancelled),
            }
        })
    }

    fn record_body(
        &self,
        _fire: usize,
        _at: jiff::Timestamp,
        _entry: Option<dal_core::EntryId>,
    ) -> Option<StreamWatchRecord> {
        None
    }
}

fn lock<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

type Reminders = Vec<(Box<str>, Box<str>)>;

/// Collects every reminder and assistant text in journal order.
fn journaled_texts(view: &View) -> (Reminders, String) {
    let mut reminders = Vec::new();
    let mut assistant = String::new();
    for entry in &view.entries.items {
        match &entry.kind {
            EntryKind::Reminder { source, text } => {
                reminders.push((source.clone(), text.clone()));
            }
            EntryKind::Assistant { content, .. } => {
                for block in content {
                    if let Block::Text { text, .. } = block {
                        assistant.push_str(text);
                    }
                }
            }
            _ => {}
        }
    }
    (reminders, assistant)
}

/// The turn's update log: rule fires, terminal stop, and every delta text.
#[derive(Default)]
struct TurnLog {
    rule_fires: Vec<Box<str>>,
    delta_text: String,
    stop: Option<Stop>,
    failed_notices: Vec<Box<str>>,
}

async fn drive_turn(subscription: &mut dal_agent::Subscription, expected: Stop) -> TurnLog {
    let mut log = TurnLog::default();
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let delivery = subscription
                .next()
                .await
                .expect("subscription closed before the turn ended");
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RuleFired { rule, .. } => log.rule_fires.push(rule.clone()),
                UpdateKind::Delta { text, .. } => log.delta_text.push_str(text),
                UpdateKind::TurnEnded { stop, .. } => {
                    log.stop = Some(*stop);
                    break;
                }
                UpdateKind::Notice(notice)
                    if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
                {
                    log.failed_notices.push(notice.text.clone());
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the turn must reach {expected:?} through the real driver and actor"
    );
    assert!(
        log.failed_notices.is_empty(),
        "terminal failures stay visible: {:?}",
        log.failed_notices
    );
    assert_eq!(log.stop, Some(expected));
    log
}

async fn start_session(extension: Extension) -> (tempfile::TempDir, Host, dal_agent::Agent) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let config_text = format!("approval = \"all\"\nmodel = \"{MODEL}\"\n");
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
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
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace");
    let agent = host
        .open(
            SessionRef::New {
                workspace,
                name: None,
            },
            ClientId::new("watch-lifecycle"),
        )
        .await
        .expect("session");
    (tmp, host, agent)
}

#[tokio::test]
async fn judged_wait_starts_only_after_the_provider_stream_is_destroyed() {
    let (gate, mut entered, verdict_tx) = JudgedGate::armed();
    let wire = ReleaseWire::default();
    let probe = Arc::new(DropProbe::new(wire.clone()));
    let (_tmp, host, agent) =
        start_session(drop_model_extension(gate, wire.clone(), Arc::clone(&probe))).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Exercise the judged stream release.".into(),
            }],
        })
        .await
        .expect("prompt");
    let Reply::Accepted { .. } = reply else {
        panic!("prompt not accepted: {reply:?}");
    };

    // The watcher only signals entry after the interrupting stream's own
    // drop notification fired: a driver that judges while the stream is
    // still attached never reaches this line and the bounded wait fails.
    tokio::time::timeout(Duration::from_secs(10), &mut entered)
        .await
        .expect("the judged wait must start after the stream drop")
        .expect("entry signal");
    assert!(
        wire.flag(),
        "the interrupting stream reported its own destruction before judgment"
    );

    // While the judged wait pends, nothing from the interrupted attempt may
    // be settled: no assistant record, no reminder, no rule fire.
    let view = agent.view(PageReq::default()).expect("view");
    let (reminders, assistant) = journaled_texts(&view);
    assert!(
        reminders.is_empty(),
        "no reminder may settle before the judged verdict: {reminders:?}"
    );
    assert!(
        assistant.is_empty(),
        "the interrupted response must not journal before the verdict: {assistant:?}"
    );

    let _ = verdict_tx.send(true);
    let log = drive_turn(&mut subscription, Stop::EndTurn).await;
    assert_eq!(
        log.rule_fires,
        vec![Box::<str>::from(RULE)],
        "the accepted judged fire publishes exactly one rule fire"
    );
    assert!(
        !log.delta_text.contains("tail after the interrupt"),
        "events after the interrupt stay discarded"
    );

    let view = agent.view(PageReq::default()).expect("view");
    let (reminders, assistant) = journaled_texts(&view);
    let sources: Vec<_> = reminders
        .iter()
        .map(|(source, _)| source.as_ref())
        .collect();
    assert_eq!(
        sources,
        vec![format!("rule:{RULE}")],
        "the accepted verdict retries with exactly one reminder"
    );
    assert_eq!(
        assistant, "clean answer after judgment",
        "the retry answer is the model's next stream, never the discarded attempt"
    );
    assert_eq!(
        probe.opened.load(Ordering::SeqCst),
        2,
        "the interrupting stream and the retry are the only model calls"
    );
    host.close(view.session.id).await.expect("session close");
}

#[tokio::test]
async fn cancel_during_the_judged_wait_delivers_no_late_fire_or_retry() {
    let (gate, mut entered, verdict_tx) = JudgedGate::armed();
    let wire = ReleaseWire::default();
    let probe = Arc::new(DropProbe::new(wire.clone()));
    let (_tmp, host, agent) =
        start_session(drop_model_extension(gate, wire.clone(), Arc::clone(&probe))).await;
    let mut subscription = agent.subscribe(None).expect("subscription");
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Cancel inside the judged wait.".into(),
            }],
        })
        .await
        .expect("prompt");
    let Reply::Accepted { turn, .. } = reply else {
        panic!("prompt not accepted: {reply:?}");
    };

    tokio::time::timeout(Duration::from_secs(10), &mut entered)
        .await
        .expect("the judged wait must start after the stream drop")
        .expect("entry signal");
    assert!(wire.flag());

    agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .expect("cancel accepted");
    // Release a late acceptance after the cancel: the driver must already
    // have left the judged wait, so nothing may fire or retry.
    let _ = verdict_tx.send(true);

    let log = drive_turn(&mut subscription, Stop::Cancelled).await;
    assert!(
        log.rule_fires.is_empty(),
        "a cancelled judged fire must not publish a late rule fire: {:?}",
        log.rule_fires
    );
    assert!(
        !log.delta_text.contains("clean answer after judgment"),
        "a cancelled turn must not run the retry response"
    );

    let view = agent.view(PageReq::default()).expect("view");
    let (reminders, assistant) = journaled_texts(&view);
    assert!(
        reminders.is_empty(),
        "no late reminder may be appended after cancellation: {reminders:?}"
    );
    assert!(
        assistant.is_empty(),
        "the cancelled attempt leaves no assistant record: {assistant:?}"
    );

    // The probe's second stream must still belong to the NEXT request: a
    // late retry would have consumed it and this prompt would get nothing.
    let opened_before = probe.opened.load(Ordering::SeqCst);
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "The next request owns the remaining stream.".into(),
            }],
        })
        .await
        .expect("prompt");
    let Reply::Accepted { .. } = reply else {
        panic!("follow-up prompt not accepted: {reply:?}");
    };
    let log = drive_turn(&mut subscription, Stop::EndTurn).await;
    assert!(
        log.rule_fires.is_empty() && !log.delta_text.is_empty(),
        "the follow-up turn consumes the probe's next stream, proving no late retry ran"
    );
    let opened_after = probe.opened.load(Ordering::SeqCst);
    assert_eq!(
        opened_after,
        opened_before + 1,
        "exactly one new stream serves the follow-up prompt"
    );
    let view = agent.view(PageReq::default()).expect("view");
    let (_, assistant) = journaled_texts(&view);
    assert_eq!(
        assistant, "clean answer after judgment",
        "the follow-up answer is the stream the cancelled turn never consumed"
    );
    host.close(view.session.id).await.expect("session close");
}
