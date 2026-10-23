#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! End-to-end wiring of the repetitive-turns lane through the real ttsr
//! extension: three similar settled replies park one lane fire, the next
//! request's watch delivers it through the stream-fire path — the
//! `rule:repetitive-turns` reminder entry, the `RuleFired` record, the `ttsr`
//! `rule_fired` Ext record, the `RuleFired` update, and the gate latch — and
//! the first request issued after that journal write carries the reminder
//! text exactly once as a user part. Dissimilar replies never fire, a
//! close/reopen starts clean and drops any undelivered fire, a cancelled
//! turn retains the fire for the next turn, and with `lane: false` nothing
//! fires.
//!
//! The model is a deterministic in-process handler serving each turn's reply
//! from a queue and capturing every `ModelRequest.context` (the lane digests
//! replies, not prompts, so the queue is what makes a turn "similar" or
//! "dissimilar"). No network, no model readiness call: the judge gate
//! resolves off with no judge model configured, which is the session-open
//! policy tested elsewhere and untouched here.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, HookCx, HookError, ModelCx, ModelError, ModelHandler,
    ModelRecord, ObserveHook,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::{SessionEnd, SessionStart, Settled};
use dal_core::{
    CancelScope, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryId, EntryKind,
    Expect, ModelId, ModelRequest, PageReq, Part, RawJson, Record, ServiceSet, Stop, ThinkingLevel,
    UpdateKind, View, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use futures::StreamExt;

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(20);

/// The rule name the lane fires under, as the journal and update carry it.
const LANE_RULE_NAME: &str = "repetitive-turns";

/// The reminder source the lane journals, the same `rule:` prefix every
/// stream fire uses. The old `hook:before_turn` source must never appear.
const LANE_SOURCE: &str = "rule:repetitive-turns";

/// The extension that owns the lane's `rule_fired` Ext records.
const LANE_EXT: &str = "ttsr";

/// Replies with pairwise digest similarity at or above the lane threshold,
/// borrowed from the lane unit-test fixtures.
const SIMILAR: [&str; 3] = [
    "I will run the cargo build again and check the failing test output.",
    "Let me run the cargo build again and check the failing test output now.",
    "I will run the cargo build again, then check the failing test output.",
];

/// Four replies with pairwise digest similarity well below the threshold.
const DISSIMILAR: [&str; 4] = [
    "The README describes installation steps for Linux and macOS users.",
    "Patching the parser module so tokens carry their byte spans.",
    "Deploying the staging cluster and migrating the database schema tonight.",
    "Reviewing the pull request comments about the flaky integration suite.",
];

/// What the lifecycle observed across the whole test.
#[derive(Default)]
struct Seen {
    settleds: usize,
    session_ends: usize,
    rule_fired: usize,
}

struct Probe(Arc<Mutex<Seen>>);

impl ObserveHook<SessionStart> for Probe {
    fn call(&self, _input: SessionStart, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

impl ObserveHook<Settled> for Probe {
    fn call(&self, _input: Settled, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        if let Ok(mut seen) = self.0.lock() {
            seen.settleds += 1;
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

impl ObserveHook<SessionEnd> for Probe {
    fn call(&self, _input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        if let Ok(mut seen) = self.0.lock() {
            seen.session_ends += 1;
        }
        Box::pin(std::future::ready(Ok(())))
    }
}

/// The deterministic model: records every request's user text parts, streams
/// the next queued reply, then ends the turn. With `block` set, it parks
/// mid-stream after the reply instead of stopping, until the turn is
/// cancelled. The test queues exactly one reply per turn.
struct CapturingReplies {
    replies: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<Vec<String>>>>,
    block: Arc<AtomicBool>,
}

impl CapturingReplies {
    fn user_text_parts(request: &ModelRequest) -> Vec<String> {
        let mut parts = Vec::new();
        for item in request.context.iter() {
            if let ContextItem::User { parts: user_parts } = item {
                for part in user_parts {
                    if let Part::Text { text } = part {
                        parts.push(text.to_string());
                    }
                }
            }
        }
        parts
    }
}

impl ModelHandler for CapturingReplies {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(Self::user_text_parts(&request));
        }
        let blocking = self.block.load(Ordering::SeqCst);
        let text = self
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .expect("the test queues one reply per turn");
        Box::pin(async move {
            let mut events: Vec<Result<StreamEvent, ProviderError>> = vec![
                Ok(StreamEvent::TextDelta { text }),
                Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
            ];
            if blocking {
                // Park mid-stream; only cancelling the turn ends it.
                let parked = futures::stream::pending::<Result<StreamEvent, ProviderError>>();
                return Ok(EventStream::new(
                    futures::stream::iter(events).chain(parked),
                    || {},
                ));
            }
            events.push(Ok(StreamEvent::Usage {
                usage: dal_core::Usage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    output_tokens: 5,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    premium_requests: None,
                    cost_usd: None,
                },
            }));
            events.push(Ok(StreamEvent::Stop {
                reason: StopReason::EndTurn,
            }));
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-ttsr-lifecycle-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn extensions_with_lanes(
    seen: &Arc<Mutex<Seen>>,
    replies: &Arc<Mutex<VecDeque<String>>>,
    requests: &Arc<Mutex<Vec<Vec<String>>>>,
    block: &Arc<AtomicBool>,
    lane: bool,
    lanes: Option<Arc<dal_ext::ttsr::lane::Lanes>>,
) -> Outcome<Vec<dal_agent::ext::Extension>> {
    // The unit under test: the real ttsr extension with its lane hooks.
    let pick_edit_style =
        Arc::new(|_route: &dal_core::ModelRoute| dal_ext::ttsr::readers::EditStyle::Replace);
    let ttsr = dal_ext::ttsr::extension_with_lanes(
        dal_ext::ttsr::TtsrDeps {
            pick_edit_style,
            lane,
        },
        lanes,
    )?;
    let probe = ExtensionBuilder::new("lane-probe", "0.1.0", ServiceSet::EMPTY)?
        .on_session_start(Probe(Arc::clone(seen)))
        .on_settled(Probe(Arc::clone(seen)))
        .on_session_end(Probe(Arc::clone(seen)))
        .build()?;
    let model = ExtensionBuilder::new("fixture-model", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("fixture/end")?,
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(CapturingReplies {
                replies: Arc::clone(replies),
                requests: Arc::clone(requests),
                block: Arc::clone(block),
            }),
            export: None,
        })
        .build()?;
    Ok(vec![ttsr, probe, model])
}

async fn start_host(
    scratch: &Scratch,
    seen: &Arc<Mutex<Seen>>,
    replies: &Arc<Mutex<VecDeque<String>>>,
    requests: &Arc<Mutex<Vec<Vec<String>>>>,
    block: &Arc<AtomicBool>,
    lane: bool,
) -> Outcome<(Host, Workspace)> {
    start_host_with_lanes(scratch, seen, replies, requests, block, lane, None).await
}

async fn start_host_with_lanes(
    scratch: &Scratch,
    seen: &Arc<Mutex<Seen>>,
    replies: &Arc<Mutex<VecDeque<String>>>,
    requests: &Arc<Mutex<Vec<Vec<String>>>>,
    block: &Arc<AtomicBool>,
    lane: bool,
    lanes: Option<Arc<dal_ext::ttsr::lane::Lanes>>,
) -> Outcome<(Host, Workspace)> {
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: extensions_with_lanes(seen, replies, requests, block, lane, lanes)?,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (
                OsString::from("HOME"),
                OsString::from(scratch.0.join("home")),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(scratch.0.join("cache")),
            ),
        ]),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    Ok((host, Workspace::new(workspace_dir).expect("workspace")))
}

fn session_id(agent: &Agent) -> dal_core::SessionId {
    agent.view(PageReq::default()).expect("view").session.id
}

fn queue(replies: &[&str]) -> Arc<Mutex<VecDeque<String>>> {
    Arc::new(Mutex::new(
        replies.iter().map(|text| (*text).to_owned()).collect(),
    ))
}

/// Submits one prompt and drains the subscription until the turn ends
/// cleanly and the settled dispatch has run. The settled barrier (the probe
/// count; the ttsr settled hook precedes the probe in the fanout) also
/// guarantees the watch settle that journals any lane fire has completed
/// before the next turn starts, so journal and view reads below are settled.
async fn run_turn(
    agent: &Agent,
    subscription: &mut Subscription,
    seen: &Arc<Mutex<Seen>>,
    settled_target: usize,
    text: &str,
) -> Outcome<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    let mut ended = false;
    while !ended {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-turn".into());
        };
        match &update.kind {
            UpdateKind::TurnEnded { stop, .. } => {
                assert_eq!(*stop, Stop::EndTurn, "the turn ends cleanly");
                ended = true;
            }
            UpdateKind::RuleFired { rule, .. } => {
                assert_eq!(&**rule, LANE_RULE_NAME);
                if let Ok(mut seen) = seen.lock() {
                    seen.rule_fired += 1;
                }
            }
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
            _ => {}
        }
    }
    let deadline = Instant::now() + WAIT;
    loop {
        if seen.lock().map_err(|_| "lock poisoned")?.settleds >= settled_target {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("settled dispatch not observed".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Starts one turn whose handler parks mid-stream and cancels it once it is
/// running. The cancelled turn settles nothing: no settled dispatch, no
/// journal write, no delivery of a parked lane fire.
async fn run_cancelled_turn(
    agent: &Agent,
    subscription: &mut Subscription,
    block: &Arc<AtomicBool>,
) -> Outcome<()> {
    block.store(true, Ordering::SeqCst);
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "hold".into(),
            }],
        })
        .await?;
    let mut started = false;
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "cancel turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-cancel".into());
        };
        match &update.kind {
            UpdateKind::TurnStarted { turn, .. } => {
                started = true;
                agent
                    .submit(Command::Cancel {
                        scope: CancelScope::Turn(*turn),
                    })
                    .await?;
            }
            UpdateKind::TurnEnded { stop, .. } => {
                assert_eq!(*stop, Stop::Cancelled, "the blocked turn was cancelled");
                break;
            }
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
            _ => {}
        }
    }
    block.store(false, Ordering::SeqCst);
    if !started {
        return Err("the cancelled turn never started".into());
    }
    Ok(())
}

/// Waits for the `n`th session end; the ttsr hook shares the probe fanout.
async fn wait_session_ends(seen: &Arc<Mutex<Seen>>, n: usize) -> Outcome<()> {
    let deadline = Instant::now() + WAIT;
    loop {
        if seen.lock().map_err(|_| "lock poisoned")?.session_ends >= n {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("session_end not observed".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn lane_reminders(view: &View) -> Vec<(EntryId, String)> {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } if source.as_ref() == LANE_SOURCE => {
                Some((entry.id, text.to_string()))
            }
            _ => None,
        })
        .collect()
}

fn before_turn_reminders(view: &View) -> Vec<String> {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } if source.as_ref() == "hook:before_turn" => {
                Some(text.to_string())
            }
            _ => None,
        })
        .collect()
}

/// Every durable journal record across the scratch data root.
fn journal_records(data: &Path) -> Outcome<Vec<Record>> {
    let mut records = Vec::new();
    let mut pending = vec![data.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for found in std::fs::read_dir(&directory)? {
            let path = found?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
                let text = std::fs::read_to_string(&path)?;
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    records.push(dal_core::decode(line.as_bytes())?.record);
                }
            }
        }
    }
    Ok(records)
}

/// The entry ids of the lane's `RuleFired` records.
fn journal_rule_fired(records: &[Record]) -> Vec<EntryId> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::RuleFired { rule, entry, .. } if rule.as_ref() == LANE_RULE_NAME => {
                Some(*entry)
            }
            _ => None,
        })
        .collect()
}

/// The decoded bodies of the ttsr `rule_fired` Ext records.
fn journal_lane_ext_bodies(records: &[Record]) -> Outcome<Vec<BTreeMap<String, RawJson>>> {
    let mut bodies = Vec::new();
    for record in records {
        let Record::Ext {
            ext, kind, body, ..
        } = record
        else {
            continue;
        };
        if ext.as_ref() != LANE_EXT || kind.as_ref() != "rule_fired" {
            continue;
        }
        bodies.push(body.decode_as()?);
    }
    Ok(bodies)
}

/// Whether any journal record carries the old `hook:before_turn` reminder.
fn journal_has_before_turn(records: &[Record]) -> bool {
    records.iter().any(|record| {
        matches!(
            record,
            Record::Reminder(entry)
                if matches!(
                    &entry.kind,
                    dal_core::EntryKind::Reminder { source, .. }
                        if source.as_ref() == "hook:before_turn"
                )
        )
    })
}

/// How many user text parts of the `index`th captured request equal `text`.
fn request_user_text_count(requests: &[Vec<String>], index: usize, text: &str) -> usize {
    requests[index]
        .iter()
        .filter(|part| part.as_str() == text)
        .count()
}

/// The exact reminder text the lane delivers: the built-in rule rendered
/// with subject `reply`, byte for byte the texts module's projection.
fn expected_reminder() -> String {
    dal_ext::ttsr::texts::render_reminder_text(
        dal_ext::ttsr::lane::lane_rule(),
        dal_ext::ttsr::texts::RuleSubject::Reply,
    )
}

/// Asserts the delivered state for exactly `count` lane fires: the view's
/// reminder entries (id, text), the `RuleFired` records' entries, the Ext
/// record bodies, the observed update count, and no `hook:before_turn`
/// anywhere. `excerpt_replies` names, in delivery order, the reply that
/// fired each time, so every parked excerpt is paired with its own source
/// reply; a fire reusing an older excerpt fails its own entry's check.
fn assert_lane_fires(
    view: &View,
    records: &[Record],
    seen: &Seen,
    count: usize,
    excerpt_replies: &[&str],
) -> Outcome<()> {
    assert_eq!(
        excerpt_replies.len(),
        count,
        "one source reply per expected fire"
    );
    let reminders = lane_reminders(view);
    assert_eq!(
        reminders.len(),
        count,
        "exactly one reminder entry per delivered fire: {reminders:?}"
    );
    let fired = journal_rule_fired(records);
    assert_eq!(
        fired.len(),
        count,
        "exactly one RuleFired record per delivered fire"
    );
    let bodies = journal_lane_ext_bodies(records)?;
    assert_eq!(
        bodies.len(),
        count,
        "exactly one ttsr rule_fired Ext record per delivered fire"
    );
    assert_eq!(
        seen.rule_fired, count,
        "exactly one RuleFired update per delivered fire"
    );
    assert!(
        before_turn_reminders(view).is_empty(),
        "no hook:before_turn reminder in the view"
    );
    assert!(
        !journal_has_before_turn(records),
        "no hook:before_turn record"
    );
    let expected = expected_reminder();
    for ((id, text), entry) in reminders.iter().zip(&fired) {
        assert_eq!(text, &expected, "the rendered reminder text");
        assert_eq!(id, entry, "the RuleFired record names the reminder entry");
    }
    for (body, source_reply) in bodies.iter().zip(excerpt_replies) {
        let field = |name: &str| -> Outcome<String> {
            body.get(name)
                .map(dal_core::RawJson::decode_as)
                .transpose()?
                .ok_or_else(|| format!("the rule_fired body lacks {name}").into())
        };
        assert_eq!(field("action")?, "remind");
        assert_eq!(field("subject")?, "reply");
        assert_eq!(field("pattern")?, LANE_RULE_NAME);
        assert_eq!(field("origin")?, "plugin:ttsr");
        let excerpt = field("excerpt")?;
        assert!(!excerpt.is_empty(), "the fire carries a reply excerpt");
        assert!(
            source_reply.ends_with(&excerpt),
            "the excerpt is a suffix of the reply that fired: {excerpt:?}"
        );
        let entry: u64 = body
            .get("entry")
            .map(dal_core::RawJson::decode_as::<u64>)
            .transpose()?
            .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                "the rule_fired body lacks entry".into()
            })?;
        assert!(
            fired.iter().any(|id| id.get() == entry),
            "the Ext record names a journaled reminder entry"
        );
        let turn: u64 = body
            .get("turn")
            .map(dal_core::RawJson::decode_as::<u64>)
            .transpose()?
            .ok_or_else(|| -> Box<dyn std::error::Error + Send + Sync> {
                "the rule_fired body lacks turn".into()
            })?;
        assert!(
            turn > 0,
            "the Ext record carries the persisted numeric turn"
        );
    }
    Ok(())
}

#[tokio::test]
async fn lane_fire_uses_the_stream_fire_path_and_reaches_the_model() -> Outcome<()> {
    let scratch = Scratch::new("fire");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // Five similar replies: the fire lands on turn 3, turn 4 journals it,
    // turn 5 sees it and stays latched.
    let replies = queue(&[SIMILAR[0], SIMILAR[1], SIMILAR[2], SIMILAR[0], SIMILAR[1]]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-fire"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    // Turns 1 and 2: similar, below the streak of 3, nothing fires.
    run_turn(&agent, &mut subscription, &seen, 1, "un").await?;
    run_turn(&agent, &mut subscription, &seen, 2, "deux").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;

    // Turn 3 completes the streak; turn 4's watch delivers the fire.
    run_turn(&agent, &mut subscription, &seen, 3, "trois").await?;
    run_turn(&agent, &mut subscription, &seen, 4, "quatre").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;
    // Turn 5: the lane stays latched while the streak holds; no second fire.
    run_turn(&agent, &mut subscription, &seen, 5, "cinq").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;
    // The journal write landed during turn 4, so turn 5's request — the
    // first issued after it — carries the reminder text exactly once, as a
    // user part.
    {
        let captured = requests.lock().map_err(|_| "lock poisoned")?;
        assert_eq!(
            request_user_text_count(&captured, 4, &expected_reminder()),
            1,
            "the next request carries the reminder text exactly once"
        );
    }

    host.close(session_id(&agent)).await?;
    wait_session_ends(&seen, 1).await?;
    assert_eq!(seen.lock().map_err(|_| "lock poisoned")?.settleds, 5);
    Ok(())
}

#[tokio::test]
async fn latch_holds_until_a_dissimilar_turn_rearms() -> Outcome<()> {
    let scratch = Scratch::new("latch");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // Fire on turn 3 (delivered at 4); turns 4-5 stay latched; turn 6
    // re-arms; the second fire lands on turn 8 (delivered at 9). The
    // re-arming turn restarts the streak at one, so two further similar
    // turns complete it — the lane unit contract — and the two firing
    // turns serve distinct replies, so each parked excerpt must name its
    // own firing reply.
    let replies = queue(&[
        SIMILAR[0],
        SIMILAR[1],
        SIMILAR[2],
        SIMILAR[0],
        SIMILAR[1],
        DISSIMILAR[0],
        SIMILAR[0],
        SIMILAR[1],
        SIMILAR[2],
        SIMILAR[1],
        SIMILAR[0],
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-latch"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    for (target, text) in [
        (1, "un"),
        (2, "deux"),
        (3, "trois"),
        (4, "quatre"),
        (5, "cinq"),
        (6, "six"),
    ] {
        run_turn(&agent, &mut subscription, &seen, target, text).await?;
    }
    // One delivered fire so far (the turn-3 streak; turn 6 re-armed).
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;

    for (target, text) in [(7, "sept"), (8, "huit"), (9, "neuf"), (10, "dix")] {
        run_turn(&agent, &mut subscription, &seen, target, text).await?;
    }
    // The re-armed streak fires exactly once more: similar turns 7-8
    // complete the restarted streak, and turn 9's watch delivers it.
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        2,
        &[SIMILAR[2], SIMILAR[1]],
    )?;
    {
        let captured = requests.lock().map_err(|_| "lock poisoned")?;
        assert_eq!(
            request_user_text_count(&captured, 8, &expected_reminder()),
            1,
            "the request before the second journal write still carries only the first reminder"
        );
        assert_eq!(
            request_user_text_count(&captured, 9, &expected_reminder()),
            2,
            "the request after the second journal write carries both reminders: one retained plus exactly one new"
        );
    }
    // One trailing turn: nothing fires again while the streak holds.
    run_turn(&agent, &mut subscription, &seen, 11, "onze").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        2,
        &[SIMILAR[2], SIMILAR[1]],
    )?;
    host.close(session_id(&agent)).await?;
    Ok(())
}

#[tokio::test]
async fn dissimilar_replies_never_fire_the_lane() -> Outcome<()> {
    let scratch = Scratch::new("dissimilar");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // Four dissimilar replies plus one trailing turn past every possible
    // fire point.
    let replies = queue(&[
        DISSIMILAR[0],
        DISSIMILAR[1],
        DISSIMILAR[2],
        DISSIMILAR[3],
        DISSIMILAR[0],
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-dissimilar"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    for (target, text) in [
        (1, "un"),
        (2, "deux"),
        (3, "trois"),
        (4, "quatre"),
        (5, "cinq"),
    ] {
        run_turn(&agent, &mut subscription, &seen, target, text).await?;
    }
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;
    let reminder = expected_reminder();
    assert!(
        requests
            .lock()
            .map_err(|_| "lock poisoned")?
            .iter()
            .all(|parts| parts.iter().all(|part| part != &reminder)),
        "no request ever carries the reminder text"
    );

    host.close(session_id(&agent)).await?;
    Ok(())
}

#[tokio::test]
async fn session_close_reopen_starts_with_clean_repetition_state() -> Outcome<()> {
    let scratch = Scratch::new("reopen");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // Two similar replies before the close; if the lane state survived, the
    // first similar reply after the reopen would complete the streak and a
    // later turn would deliver it.
    let replies = queue(&[
        SIMILAR[0],
        SIMILAR[1],
        SIMILAR[2],
        SIMILAR[0],
        DISSIMILAR[0],
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-reopen"),
        )
        .await?;
    let session = session_id(&agent);
    let data = scratch.0.join("data");

    let mut subscription = agent.subscribe(None)?;
    run_turn(&agent, &mut subscription, &seen, 1, "un").await?;
    run_turn(&agent, &mut subscription, &seen, 2, "deux").await?;
    drop(subscription);
    host.close(session).await?;
    wait_session_ends(&seen, 1).await?;

    let resumed = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: workspace.clone(),
            },
            ClientId::new("lane-reopen-2"),
        )
        .await?;
    assert_eq!(session_id(&resumed), session);

    let mut subscription = resumed.subscribe(None)?;
    run_turn(&resumed, &mut subscription, &seen, 3, "trois").await?;
    run_turn(&resumed, &mut subscription, &seen, 4, "quatre").await?;
    // One trailing turn past every possible fire point.
    run_turn(&resumed, &mut subscription, &seen, 5, "cinq").await?;
    let view = resumed.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;
    host.close(session).await?;
    Ok(())
}

#[tokio::test]
async fn undelivered_fire_is_dropped_at_close() -> Outcome<()> {
    let scratch = Scratch::new("reopen-pending");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // Three similar replies leave a fire pending; the close evicts it, so
    // the resumed session delivers nothing even though its turn is similar.
    let replies = queue(&[SIMILAR[0], SIMILAR[1], SIMILAR[2], SIMILAR[0]]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-pending"),
        )
        .await?;
    let session = session_id(&agent);
    let data = scratch.0.join("data");

    let mut subscription = agent.subscribe(None)?;
    run_turn(&agent, &mut subscription, &seen, 1, "un").await?;
    run_turn(&agent, &mut subscription, &seen, 2, "deux").await?;
    run_turn(&agent, &mut subscription, &seen, 3, "trois").await?;
    drop(subscription);
    host.close(session).await?;
    wait_session_ends(&seen, 1).await?;

    let resumed = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: workspace.clone(),
            },
            ClientId::new("lane-pending-2"),
        )
        .await?;
    assert_eq!(session_id(&resumed), session);

    let mut subscription = resumed.subscribe(None)?;
    run_turn(&resumed, &mut subscription, &seen, 4, "quatre").await?;
    let view = resumed.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;
    host.close(session).await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_turn_retains_the_fire_until_the_next_one() -> Outcome<()> {
    let scratch = Scratch::new("cancel");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // The fire lands on turn 3; the blocked turn is cancelled mid-stream and
    // must not deliver it; the next turn delivers exactly once, and the
    // request after that carries the text.
    let replies = queue(&[
        SIMILAR[0], SIMILAR[1], SIMILAR[2], SIMILAR[0], SIMILAR[1], SIMILAR[2],
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, true).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-cancel"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    run_turn(&agent, &mut subscription, &seen, 1, "un").await?;
    run_turn(&agent, &mut subscription, &seen, 2, "deux").await?;
    run_turn(&agent, &mut subscription, &seen, 3, "trois").await?;

    // The blocked turn is cancelled mid-stream: nothing is journaled.
    run_cancelled_turn(&agent, &mut subscription, &block).await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;

    // The following turn delivers exactly the one retained fire.
    run_turn(&agent, &mut subscription, &seen, 4, "quatre").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;
    // One trailing turn: the request issued after the retained fire journals
    // carries the text exactly once, and nothing fires again.
    run_turn(&agent, &mut subscription, &seen, 5, "cinq").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;
    {
        let captured = requests.lock().map_err(|_| "lock poisoned")?;
        assert_eq!(
            request_user_text_count(&captured, 3, &expected_reminder()),
            0,
            "the cancelled turn's request never carried the text"
        );
        assert_eq!(
            request_user_text_count(&captured, 5, &expected_reminder()),
            1,
            "the request after the retained fire journals carries the text once"
        );
    }

    host.close(session_id(&agent)).await?;
    Ok(())
}

#[tokio::test]
async fn lane_off_composes_nothing() -> Outcome<()> {
    let scratch = Scratch::new("lane-off");
    let seen = Arc::new(Mutex::new(Seen::default()));
    // With `lane: false`, even a full streak plus a trailing turn produces
    // no lane entry, record, or update.
    let replies = queue(&[SIMILAR[0], SIMILAR[1], SIMILAR[2], SIMILAR[0], SIMILAR[1]]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let (host, workspace) = start_host(&scratch, &seen, &replies, &requests, &block, false).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("lane-off"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    for (target, text) in [
        (1, "un"),
        (2, "deux"),
        (3, "trois"),
        (4, "quatre"),
        (5, "cinq"),
    ] {
        run_turn(&agent, &mut subscription, &seen, target, text).await?;
    }
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;
    let reminder = expected_reminder();
    assert!(
        requests
            .lock()
            .map_err(|_| "lock poisoned")?
            .iter()
            .all(|parts| parts.iter().all(|part| part != &reminder)),
        "no request ever carries the reminder text"
    );

    host.close(session_id(&agent)).await?;
    Ok(())
}

#[tokio::test]
async fn ttsr_intervention_triggers_at_threshold_and_cleans_up_on_session_end() -> Outcome<()> {
    let scratch = Scratch::new("threshold-cleanup");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let replies = queue(&[SIMILAR[0], SIMILAR[1], SIMILAR[2], SIMILAR[0]]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block = Arc::new(AtomicBool::new(false));
    let lanes = Arc::new(dal_ext::ttsr::lane::Lanes::new());

    assert_eq!(lanes.session_count(), 0);

    let (host, workspace) = start_host_with_lanes(
        &scratch,
        &seen,
        &replies,
        &requests,
        &block,
        true,
        Some(Arc::clone(&lanes)),
    )
    .await?;

    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("life-threshold-cleanup"),
        )
        .await?;
    let session = session_id(&agent);
    let mut subscription = agent.subscribe(None)?;
    let data = scratch.0.join("data");

    // Turn 1 and 2: below the streak threshold of 3, nothing fires.
    run_turn(&agent, &mut subscription, &seen, 1, "un").await?;
    assert_eq!(lanes.session_count(), 1);
    run_turn(&agent, &mut subscription, &seen, 2, "deux").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(&view, &records, &*seen.lock().map_err(|_| "lock")?, 0, &[])?;

    // Turn 3: completes configured streak threshold (LANE_STREAK = 3) -> intervention fires!
    run_turn(&agent, &mut subscription, &seen, 3, "trois").await?;
    // Turn 4: delivers the intervention
    run_turn(&agent, &mut subscription, &seen, 4, "quatre").await?;
    let view = agent.view(PageReq::default())?;
    let records = journal_records(&data)?;
    assert_lane_fires(
        &view,
        &records,
        &*seen.lock().map_err(|_| "lock")?,
        1,
        &[SIMILAR[2]],
    )?;

    // Close session: per-session state cleans up completely
    drop(subscription);
    host.close(session).await?;
    wait_session_ends(&seen, 1).await?;

    // Assert per-session state is completely cleaned up
    assert_eq!(
        lanes.session_count(),
        0,
        "lane window evicted on session end"
    );
    assert_eq!(
        lanes.pending(session),
        None,
        "pending fire excerpt cleared on session end"
    );

    // Host shutdown: no leaked watch tasks or timers
    let report = host.shutdown(Duration::from_secs(5)).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "no leaked watch tasks or session background tasks"
    );
    assert_eq!(report.sessions_closed, 0);

    Ok(())
}
