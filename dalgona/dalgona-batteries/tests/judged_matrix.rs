// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The judged battery's behavior matrix over a real `Host`, the scripted
//! provider, and the real judge.
//!
//! Every scenario scripts the provider's replies in the order the runtime
//! consumes them: a classification call runs inside `before_request`, before
//! the turn's own request opens; an anchored-ask call runs inside `tool_call`;
//! a claim check runs inside `settled`, before the turn ends. A judge call
//! that fires when the scenario scripted none consumes a turn step instead,
//! so the assistant text, the ledger rows, and the request parameters all
//! change, and the assertions below fail.
//!
//! Ledger rows are read back from the closed session's journal, the only
//! durable trace of the battery.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx,
    ModelError, ModelHandler, ModelRecord, ObserveHook, Services,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::BeforeRequest;
use dal_core::{
    Answer, CallId, Caps, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, ModelId,
    ModelRequest, ModelRoute, Origin, PageReq, Part, Record, RegistrationError, RequestParams,
    Save, ServiceSet, SessionId, Settled, Stop, ThinkingLevel, UpdateKind, Workspace,
};
use dal_store::Store;
use dal_tools::{Rerank, RerankCall, RerankCandidate, RerankOutput};
use dalgona_batteries::ask::ask;
use dalgona_batteries::judged::{Battery, JudgedConfig, JudgedRerank, judged};
use sonic_rs::JsonValueTrait;
use tokio::fs;

const LUNA: &str = "openai/gpt-6-luna";
const ONE_LEVEL: &str = "acme/one-level";
const SLOW: &str = "acme/slow";
const WAIT: Duration = Duration::from_secs(60);
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

/// The luna model's thinking levels in the order the judge is shown:
/// off, low, medium, high, xhigh, max.
const MEDIUM: u8 = 2;
const HIGH: u8 = 3;
const MAX: u8 = 5;

const EXPLORE_FIRST: u8 = 0;
const IDEAL_STATE: u8 = 1;
const OWNER_DECISION: u8 = 2;

const BLOCK_EXPLORE_FIRST: &str = "ask anchored: the evidence you already have can answer this. Explore first, and ask again only if it stays open.";
const BLOCK_IDEAL_STATE: &str =
    "ask anchored: your goal state settles this. Take the resolving step instead of asking.";
const CLAIM_REMINDER: &str = "Your previous reply stated facts with confidence. Treat them as claims, not evidence: verify each before you build on it, or say plainly that it is unverified.";
const BREAKER_NOTICE: &str =
    "judged: auto-thinking disabled for this session after 3 failed classifications.";

type TestResult<T = ()> = Result<T, Box<dyn StdError>>;

const ASK_ONE: &str = r#"{"questions":[{"id":"why_blue","header":"Why","question":"Why is the sky blue?","kind":"text"}]}"#;
const ASK_THREE: &str = r#"{"questions":[{"id":"q_one","header":"One","question":"First?","kind":"text"},{"id":"q_two","header":"Two","question":"Second?","kind":"text"},{"id":"q_three","header":"Three","question":"Third?","kind":"text"}]}"#;
const ASK_EXPLICIT: &str = r#"{"explicit":true,"questions":[{"id":"why_blue","header":"Why","question":"Why is the sky blue?","kind":"text"}]}"#;

// ---- scripted provider steps ------------------------------------------------

fn json_string(text: &str) -> String {
    sonic_rs::to_string(text).expect("a string encodes as JSON")
}

fn text_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{}}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#,
        json_string(text)
    )
}

fn tool_step(name: &str, args: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"call-1","name":"{name}"}},{{"type":"tool_calls_done","calls":[{{"id":"call-1","name":"{name}","args":{{"kind":"parsed","value":{args}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    )
}

/// One judge reply: the typed answer array the judge's parser expects.
fn judge_step(answers: &str) -> String {
    text_step(&format!(r#"{{"answers":{answers}}}"#))
}

fn choice(index: u8) -> String {
    judge_step(&format!("[{index}]"))
}

fn verdict(value: bool) -> String {
    judge_step(&format!("[{value}]"))
}

fn scores(values: &[u8]) -> String {
    let list: Vec<String> = values.iter().map(u8::to_string).collect();
    judge_step(&format!("[{}]", list.join(",")))
}

/// A judge reply the parser rejects.
fn garbage() -> String {
    text_step("this is not a judge answer")
}

/// A reply of exactly `len` bytes of plain words.
fn reply_of(len: usize, marker: &str) -> String {
    let mut text = format!("{marker} ");
    while text.len() < len {
        text.push_str("fact ");
    }
    text.truncate(len);
    text
}

// ---- probe and synthetic-model extensions -----------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
struct Seen {
    round: u32,
    thinking: ThinkingLevel,
    explicit: bool,
    levels: usize,
}

/// Records every `before_request` the host runs after the judged battery:
/// first-party first, then by name, and `zz-probe` sorts after `judged`, so
/// the probe sees the parameters the battery chose. Its `settled` observer
/// also runs after the battery's, which makes it a barrier: the host
/// publishes `TurnEnded` before it observes `settled`, so a scenario waits
/// for the probe before it starts the next turn.
struct Probe {
    seen: Mutex<Vec<Seen>>,
    handle: Mutex<Option<(SessionId, Arc<dyn Services>)>>,
    settled: tokio::sync::watch::Sender<usize>,
}

impl Probe {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            handle: Mutex::new(None),
            settled: tokio::sync::watch::channel(0).0,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("probe lock").clone()
    }

    fn handle(&self) -> (SessionId, Arc<dyn Services>) {
        let guard = self.handle.lock().expect("probe lock");
        let (session, services) = guard.as_ref().expect("a request ran");
        (*session, Arc::clone(services))
    }

    /// Waits until the host has observed `settled` for `turns` turns.
    async fn settled(&self, turns: usize) -> TestResult {
        let mut count = self.settled.subscribe();
        tokio::time::timeout(WAIT, count.wait_for(|seen| *seen >= turns))
            .await
            .map_err(|_| "the settled observers run in time")??;
        Ok(())
    }
}

struct ProbeHook(Arc<Probe>);

impl Hook<BeforeRequest, Option<RequestParams>> for ProbeHook {
    fn call(
        &self,
        event: BeforeRequest,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        let probe = Arc::clone(&self.0);
        let session = cx.session;
        let services = Arc::clone(&cx.services);
        Box::pin(async move {
            probe.seen.lock().expect("probe lock").push(Seen {
                round: event.round,
                thinking: event.params.thinking,
                explicit: event.thinking_explicit,
                levels: event.caps.thinking.len(),
            });
            *probe.handle.lock().expect("probe lock") = Some((session, services));
            Ok(None)
        })
    }
}

struct SettledMark(Arc<Probe>);

impl ObserveHook<Settled> for SettledMark {
    fn call(&self, _event: Settled, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let probe = Arc::clone(&self.0);
        Box::pin(async move {
            probe.settled.send_modify(|count| *count += 1);
            Ok(())
        })
    }
}

fn probe_extension(probe: Arc<Probe>) -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("zz-probe", "0.1.0", ServiceSet::EMPTY)?
        .with_origin(Origin::Bundled, None)
        .on_before_request(ProbeHook(Arc::clone(&probe)))
        .on_settled(SettledMark(probe))
        .build()
}

/// A synthetic model that waits, then serves the next scripted step.
struct Forward {
    delay: Duration,
}

impl ModelHandler for Forward {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let delay = self.delay;
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            request.model = ModelRoute::from_id(LUNA);
            cx.forward(request, &[]).await
        })
    }
}

fn model_record(id: &str, levels: &[ThinkingLevel], delay: Duration) -> TestResult<ModelRecord> {
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: Caps {
            context_window: Some(100_000),
            thinking: levels.into(),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(Forward { delay }),
        export: None,
    })
}

/// `acme/one-level` offers one thinking level; `acme/slow` answers after
/// six seconds, well past the two-second classification wait.
fn models_extension() -> TestResult<Extension> {
    Ok(
        ExtensionBuilder::new("acme-models", "0.1.0", ServiceSet::EMPTY)?
            .with_origin(Origin::Bundled, None)
            .model(model_record(
                ONE_LEVEL,
                &[ThinkingLevel::Off],
                Duration::ZERO,
            )?)
            .model(model_record(
                SLOW,
                &[ThinkingLevel::Medium, ThinkingLevel::High],
                Duration::from_secs(6),
            )?)
            .build()?,
    )
}

// ---- rig --------------------------------------------------------------------

/// The all-features-off default; each scenario enables what it exercises.
const OFF: JudgedConfig = JudgedConfig {
    thinking: false,
    ranking: false,
    ask_anchor: false,
    claim_check: false,
    dedup: false,
};

struct Setup {
    script: Vec<String>,
    /// `None` runs the session without the battery (the control).
    judged: Option<JudgedConfig>,
    judge_toml: &'static str,
    session_model: &'static str,
    with_ask: bool,
}

impl Setup {
    fn new(script: Vec<String>, cfg: JudgedConfig) -> Self {
        Self {
            script,
            judged: Some(cfg),
            judge_toml: "[judge]\ngate = \"on\"\n",
            session_model: LUNA,
            with_ask: false,
        }
    }

    fn with_ask(mut self) -> Self {
        self.with_ask = true;
        self
    }

    fn judge(mut self, toml: &'static str) -> Self {
        self.judge_toml = toml;
        self
    }

    fn model(mut self, model: &'static str) -> Self {
        self.session_model = model;
        self
    }
}

struct Rig {
    host: Host,
    agent: Agent,
    subscription: Subscription,
    probe: Arc<Probe>,
    battery: Option<Arc<Battery>>,
    rerank: Option<Arc<JudgedRerank>>,
    root: PathBuf,
    data: PathBuf,
    workspace: Workspace,
    /// Turns run so far; the probe's settled count must reach it before the
    /// next prompt, because the host observes `settled` after `TurnEnded`.
    turns: usize,
}

async fn start(setup: Setup) -> TestResult<Rig> {
    let root = std::env::temp_dir().join(format!("dalgona-judged-matrix-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let workspace_dir = root.join("workspace");
    fs::create_dir_all(&data).await?;
    fs::create_dir_all(&workspace_dir).await?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, setup.script.join("\n") + "\n").await?;
    let user = format!(
        "approval = \"all\"\nmodel = \"{}\"\n\n{}\n[providers.scripted]\nfixture = \"{}\"\n",
        setup.session_model,
        setup.judge_toml,
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&user))?;
    let probe = Arc::new(Probe::new());
    let mut extensions = vec![models_extension()?, probe_extension(Arc::clone(&probe))?];
    if setup.with_ask {
        extensions.push(ask()?);
    }
    let (battery, rerank) = match setup.judged {
        Some(cfg) => {
            let parts = judged(cfg)?;
            extensions.push(parts.extension);
            (Some(parts.battery), Some(parts.rerank))
        }
        None => (None, None),
    };
    let product = Product {
        name: "dalgona",
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
    let host = Host::start(product, config, env).await?;
    let workspace = Workspace::new(workspace_dir)?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace.clone(),
                name: None,
            },
            ClientId::new("judged-matrix"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    Ok(Rig {
        host,
        agent,
        subscription,
        probe,
        battery,
        rerank,
        root,
        data,
        workspace,
        turns: 0,
    })
}

/// One settled tool call as the client sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Outcome {
    is_error: bool,
    text: String,
    /// Whether the tool ran: a call blocked before dispatch has no elapsed time.
    ran: bool,
}

struct Turn {
    outcomes: Vec<Outcome>,
    notices: Vec<(String, String)>,
    requests_opened: usize,
    stop: Stop,
    elapsed: Duration,
}

impl Turn {
    fn last(&self) -> &Outcome {
        self.outcomes.last().expect("a tool settled")
    }

    fn outcome_text(&self) -> &str {
        &self.last().text
    }
}

/// Runs one prompt turn on `agent` to its end. Front-end questions are
/// cancelled so a regression that reaches the broker still ends the turn.
async fn drive_turn(
    agent: &Agent,
    subscription: &mut Subscription,
    prompt: &str,
) -> TestResult<Turn> {
    let started = Instant::now();
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: prompt.into(),
            }],
        })
        .await?;
    let mut turn = Turn {
        outcomes: Vec::new(),
        notices: Vec::new(),
        requests_opened: 0,
        stop: Stop::EndTurn,
        elapsed: Duration::ZERO,
    };
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    turn.requests_opened += 1;
                    agent.answer(request.id, Answer::Cancel).await?;
                }
                UpdateKind::ToolSettled { outcome, .. } => {
                    turn.outcomes.push(Outcome {
                        is_error: outcome.is_error,
                        text: outcome.text.to_string(),
                        ran: outcome.elapsed_ms.is_some(),
                    });
                }
                UpdateKind::Notice(notice) => {
                    turn.notices
                        .push((notice.kind.to_string(), notice.text.to_string()));
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    turn.stop = *stop;
                    break;
                }
                _ => {}
            }
        }
        Ok::<(), Box<dyn StdError>>(())
    })
    .await
    .map_err(|_| "the turn ends in time")??;
    turn.elapsed = started.elapsed();
    Ok(turn)
}

/// Runs one prompt turn on the rig's session, then waits for every `settled`
/// observer to finish.
async fn run_turn(rig: &mut Rig, prompt: &str) -> TestResult<Turn> {
    let turn = drive_turn(&rig.agent, &mut rig.subscription, prompt).await?;
    rig.turns += 1;
    rig.probe.settled(rig.turns).await?;
    Ok(turn)
}

/// The text of the last assistant entry of the session.
fn assistant_texts(rig: &Rig) -> TestResult<Vec<String>> {
    let view = rig.agent.view(PageReq::default())?;
    Ok(view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Assistant { content, .. } => Some(
                content
                    .iter()
                    .filter_map(|block| match block {
                        dal_core::Block::Text { text, .. } => Some(text.as_ref()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect())
}

/// Every reminder entry the session journaled, as `(source, text)`.
fn reminders(rig: &Rig) -> TestResult<Vec<(String, String)>> {
    let view = rig.agent.view(PageReq::default())?;
    Ok(view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } => Some((source.to_string(), text.to_string())),
            _ => None,
        })
        .collect())
}

#[derive(Clone, Debug)]
struct Row {
    feature: String,
    status: String,
    questions: u64,
    raw: String,
}

fn rows_of(rows: &[Row], feature: &str) -> Vec<Row> {
    rows.iter()
        .filter(|row| row.feature == feature)
        .cloned()
        .collect()
}

fn statuses(rows: &[Row], feature: &str) -> Vec<String> {
    rows_of(rows, feature)
        .into_iter()
        .map(|row| row.status)
        .collect()
}

/// Closes the session and reads the judge ledger rows back from its journal.
async fn finish(rig: Rig) -> TestResult<Vec<Row>> {
    let session = rig.agent.session();
    drop(rig.subscription);
    drop(rig.agent);
    rig.host.close(session).await?;
    rig.host
        .shutdown_after_quiet_wait(Duration::from_secs(10))
        .await;
    let store = Store::new(
        rig.data.clone(),
        rig.workspace.clone(),
        dal_core::Product::Dalgona,
    );
    let (mut journal, _) = store.open_session(session).await?;
    let mut rows = Vec::new();
    for record in journal.records() {
        let Record::Ext { kind, body, .. } = record else {
            continue;
        };
        if kind.as_ref() != "judge" {
            continue;
        }
        let value: sonic_rs::Value = sonic_rs::from_str(body.as_str())?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(JsonValueTrait::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        rows.push(Row {
            feature: text("feature"),
            status: text("status"),
            questions: value
                .get("questions")
                .and_then(JsonValueTrait::as_u64)
                .unwrap_or_default(),
            raw: body.as_str().to_owned(),
        });
    }
    journal.close().await?;
    remove(&rig.root).await;
    Ok(rows)
}

async fn remove(root: &Path) {
    let _ = fs::remove_dir_all(root).await;
}

fn candidates(paths: &[&str]) -> Vec<RerankCandidate> {
    paths
        .iter()
        .map(|path| RerankCandidate {
            path: (*path).into(),
            display: format!("{path}: match").into(),
        })
        .collect()
}

/// Runs the registered reranker for the rig's session.
async fn rerank(rig: &Rig, query: &str, list: &[RerankCandidate]) -> RerankOutput {
    let (session, services) = rig.probe.handle();
    rig.rerank
        .as_ref()
        .expect("the battery runs")
        .rerank(RerankCall {
            query,
            mode: "grep",
            candidates: list,
            session,
            turn: None,
            services,
        })
        .await
}

/// A rig whose first turn opened the judge handle, with `steps` queued after
/// the turn's own reply for the direct calls that follow.
async fn rig_with_open_judge(cfg: JudgedConfig, steps: Vec<String>) -> TestResult<Rig> {
    let mut script = vec![text_step("hello")];
    script.extend(steps);
    let mut rig = start(Setup::new(script, cfg)).await?;
    run_turn(&mut rig, "open the judge").await?;
    Ok(rig)
}

fn thinking_of(seen: &[Seen]) -> Vec<ThinkingLevel> {
    seen.iter().map(|entry| entry.thinking).collect()
}

// ---- gate off ---------------------------------------------------------------

/// What one session shows the model and the user, for a byte-identity check.
#[derive(Debug, Eq, PartialEq)]
struct Surface {
    seen: Vec<Seen>,
    ask_outcome: Outcome,
    assistant: Vec<String>,
}

async fn gate_off_session(with_battery: bool) -> TestResult<(Surface, Vec<Row>)> {
    let script = vec![tool_step("ask", ASK_ONE), text_step("final")];
    let mut setup = Setup::new(script, JudgedConfig::default())
        .with_ask()
        .judge("[judge]\ngate = \"off\"\n");
    if !with_battery {
        setup.judged = None;
    }
    let mut rig = start(setup).await?;
    let turn = run_turn(&mut rig, "ask me why the sky is blue").await?;
    let ask_outcome = turn.last().clone();
    let surface = Surface {
        seen: rig.probe.seen(),
        ask_outcome,
        assistant: assistant_texts(&rig)?,
    };
    let rows = finish(rig).await?;
    Ok((surface, rows))
}

#[tokio::test]
async fn gate_off_sends_no_judge_request_and_changes_no_surface() -> TestResult {
    let (with_battery, rows) = gate_off_session(true).await?;
    let (control, _) = gate_off_session(false).await?;
    assert!(rows.is_empty(), "no judge row with the gate off: {rows:?}");
    assert_eq!(
        with_battery, control,
        "the model-visible surface is byte-identical to the battery absent"
    );
    assert!(
        !with_battery
            .ask_outcome
            .text
            .starts_with("blocked by judged"),
        "the ask is never anchored with the gate off"
    );
    Ok(())
}

// ---- auto-thinking ----------------------------------------------------------

#[tokio::test]
async fn classification_applies_to_every_round_and_writes_one_row() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    // Round 0 calls an unknown tool, so the turn needs a second request.
    let script = vec![
        choice(HIGH),
        tool_step("no_such_tool", "{}"),
        text_step("done"),
    ];
    let mut rig = start(Setup::new(script, cfg)).await?;
    let turn = run_turn(&mut rig, "refactor the module").await?;
    assert_eq!(turn.stop, Stop::EndTurn);
    let seen = rig.probe.seen();
    assert_eq!(
        seen.iter().map(|entry| entry.round).collect::<Vec<_>>(),
        [0, 1],
        "the turn made two requests"
    );
    assert_eq!(
        thinking_of(&seen),
        [ThinkingLevel::High, ThinkingLevel::High],
        "every request of the turn carries the chosen level"
    );
    assert_eq!(
        assistant_texts(&rig)?.last().map(String::as_str),
        Some("done")
    );
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "thinking"),
        ["ok"],
        "one thinking row: {rows:?}"
    );
    assert_eq!(rows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_one_level_model_is_never_judged() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    let setup = Setup::new(vec![text_step("only reply")], cfg)
        .model(ONE_LEVEL)
        .judge("[judge]\ngate = \"on\"\nmodel = \"openai/gpt-6-luna\"\n");
    let mut rig = start(setup).await?;
    run_turn(&mut rig, "hello").await?;
    let seen = rig.probe.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].levels, 1, "the model offers one level");
    assert_eq!(seen[0].thinking, ThinkingLevel::Medium, "the default stays");
    assert_eq!(
        assistant_texts(&rig)?,
        ["only reply"],
        "the scripted reply reached the turn, not a judge call"
    );
    assert!(finish(rig).await?.is_empty(), "no judge row");
    Ok(())
}

#[tokio::test]
async fn an_explicit_level_latches_manual_and_suppresses_classification() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    // Turn 1 classifies (one judge step); turn 2 must not (no judge step).
    let script = vec![
        choice(MAX),
        text_step("one"),
        text_step("two"),
        text_step("three"),
    ];
    let mut rig = start(Setup::new(script, cfg)).await?;
    run_turn(&mut rig, "first").await?;
    rig.agent
        .submit(Command::SetThinking {
            level: ThinkingLevel::Low,
            save: Save::SessionOnly,
        })
        .await?;
    run_turn(&mut rig, "second").await?;
    run_turn(&mut rig, "third").await?;
    let seen = rig.probe.seen();
    assert_eq!(
        seen.iter().map(|entry| entry.explicit).collect::<Vec<_>>(),
        [false, true, true],
        "the host reports the user's level once it is set"
    );
    assert_eq!(
        thinking_of(&seen),
        [ThinkingLevel::Max, ThinkingLevel::Low, ThinkingLevel::Low],
        "the user's level wins for the rest of the session"
    );
    assert_eq!(assistant_texts(&rig)?, ["one", "two", "three"]);
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "thinking"),
        ["ok"],
        "only the first turn is judged"
    );
    Ok(())
}

#[tokio::test]
async fn a_resumed_session_with_a_journaled_level_starts_latched() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    let script = vec![choice(MAX), text_step("one"), text_step("two")];
    let mut rig = start(Setup::new(script, cfg)).await?;
    run_turn(&mut rig, "first").await?;
    rig.agent
        .submit(Command::SetThinking {
            level: ThinkingLevel::Low,
            save: Save::SessionOnly,
        })
        .await?;
    let session = rig.agent.session();
    drop(rig.subscription);
    drop(rig.agent);
    rig.host.close(session).await?;
    rig.agent = rig
        .host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: rig.workspace.clone(),
            },
            ClientId::new("judged-matrix"),
        )
        .await?;
    rig.subscription = rig.agent.subscribe(None)?;
    run_turn(&mut rig, "second").await?;
    let seen = rig.probe.seen();
    assert_eq!(
        seen.iter().map(|entry| entry.explicit).collect::<Vec<_>>(),
        [false, true],
        "the resumed session starts latched on the journaled level"
    );
    assert_eq!(
        thinking_of(&seen),
        [ThinkingLevel::Max, ThinkingLevel::Low],
        "the journaled level wins after resume"
    );
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "thinking"),
        ["ok"],
        "only the first turn is judged"
    );
    Ok(())
}

#[tokio::test]
async fn a_slow_classification_stops_waiting_at_two_seconds_and_the_breaker_fires_once()
-> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    // The judge model answers after six seconds; the scripted provider only
    // serves the four turns.
    let script = vec![
        text_step("a"),
        text_step("b"),
        text_step("c"),
        text_step("d"),
    ];
    let mut rig = start(
        Setup::new(script, cfg)
            .judge("[judge]\ngate = \"on\"\nmodel = \"acme/slow\"\ntimeout_ms = 30000\n"),
    )
    .await?;
    let mut notices = Vec::new();
    for prompt in ["one", "two", "three", "four"] {
        let turn = run_turn(&mut rig, prompt).await?;
        assert!(
            turn.elapsed < Duration::from_millis(5_000),
            "the wait is bounded by classify_wait_ms, not the judge's six seconds: {:?}",
            turn.elapsed
        );
        notices.extend(turn.notices);
    }
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "thinking"),
        ["timeout", "timeout", "timeout"],
        "three waits end in timeouts and the fourth turn calls nothing: {rows:?}"
    );
    let breaker: Vec<_> = notices
        .iter()
        .filter(|(kind, text)| kind == "judged" && text == BREAKER_NOTICE)
        .collect();
    assert_eq!(breaker.len(), 1, "exactly one breaker notice: {notices:?}");
    Ok(())
}

#[tokio::test]
async fn three_failed_classifications_stop_all_later_calls() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    let script = vec![
        garbage(),
        text_step("a"),
        garbage(),
        text_step("b"),
        garbage(),
        text_step("c"),
        // No judge step: a fourth classification would eat this turn step.
        text_step("d"),
    ];
    let mut rig = start(Setup::new(script, cfg)).await?;
    let mut notices = Vec::new();
    for prompt in ["one", "two", "three", "four"] {
        notices.extend(run_turn(&mut rig, prompt).await?.notices);
    }
    assert_eq!(assistant_texts(&rig)?, ["a", "b", "c", "d"]);
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "thinking"), ["parse", "parse", "parse"]);
    assert_eq!(
        notices
            .iter()
            .filter(|(_, text)| text == BREAKER_NOTICE)
            .count(),
        1,
        "one notice at the third failure: {notices:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_success_between_failures_resets_the_streak() -> TestResult {
    let cfg = JudgedConfig {
        thinking: true,
        ..OFF
    };
    let script = vec![
        garbage(),
        text_step("1"),
        garbage(),
        text_step("2"),
        choice(MEDIUM),
        text_step("3"),
        garbage(),
        text_step("4"),
        garbage(),
        text_step("5"),
        choice(HIGH),
        text_step("6"),
    ];
    let mut rig = start(Setup::new(script, cfg)).await?;
    let mut notices = Vec::new();
    for prompt in ["a", "b", "c", "d", "e", "f"] {
        notices.extend(run_turn(&mut rig, prompt).await?.notices);
    }
    assert_eq!(
        thinking_of(&rig.probe.seen()).last().copied(),
        Some(ThinkingLevel::High),
        "classification is still armed on the sixth turn"
    );
    assert_eq!(assistant_texts(&rig)?, ["1", "2", "3", "4", "5", "6"]);
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "thinking"),
        ["parse", "parse", "ok", "parse", "parse", "ok"]
    );
    assert!(
        notices.iter().all(|(_, text)| text != BREAKER_NOTICE),
        "two failures, a success, two failures never trip the breaker: {notices:?}"
    );
    Ok(())
}

// ---- ranking ----------------------------------------------------------------

const RANK_NOTE_5: &str = "judged: re-ranked by relevance; 5 of 5 candidates rescored";

#[tokio::test]
async fn five_candidates_reorder_with_the_exact_note_and_one_row() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    let rig = rig_with_open_judge(cfg, vec![scores(&[1, 3, 3, 0, 2])]).await?;
    let list = candidates(&["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
    let out = rerank(&rig, "needle", &list).await;
    assert_eq!(out.order, [1, 2, 4, 0, 3], "b, c, e, a, d");
    assert_eq!(out.note.as_deref(), Some(RANK_NOTE_5));
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "search-rank"), ["ok"]);
    assert_eq!(rows_of(&rows, "search-rank")[0].questions, 5);
    Ok(())
}

#[tokio::test]
async fn equal_scores_and_a_judge_error_keep_the_input_order() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    let rig = rig_with_open_judge(cfg, vec![scores(&[2, 2, 2]), garbage()]).await?;
    let first = rerank(&rig, "equal", &candidates(&["a", "b", "c"])).await;
    assert_eq!(first.order, [0, 1, 2]);
    assert_eq!(first.note, None, "all-equal scores add no note");
    let second = rerank(&rig, "broken", &candidates(&["d", "e", "f"])).await;
    assert_eq!(second.order, [0, 1, 2]);
    assert_eq!(second.note, None, "a judge error adds no note");
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "search-rank"), ["ok", "parse"]);
    Ok(())
}

#[tokio::test]
async fn the_same_query_twice_judges_each_path_once() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    // One batch step only: a second batch would fail on the exhausted script.
    let rig = rig_with_open_judge(cfg, vec![scores(&[0, 3, 1])]).await?;
    let list = candidates(&["a", "b", "c"]);
    let first = rerank(&rig, "memo", &list).await;
    let second = rerank(&rig, "memo", &list).await;
    assert_eq!(first, second, "the memo serves the same order and note");
    assert_eq!(first.order, [1, 2, 0]);
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "search-rank"), ["ok"], "one batch in total");
    Ok(())
}

#[tokio::test]
async fn candidates_beyond_twenty_stay_ambient_and_stable() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    // Twenty answers: indices 0..10 irrelevant (0), 10..20 supportive (2).
    let mut answers = vec![0_u8; 10];
    answers.extend(vec![2_u8; 10]);
    let rig = rig_with_open_judge(cfg, vec![scores(&answers)]).await?;
    let paths: Vec<String> = (0..25).map(|index| format!("f{index:02}.rs")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let out = rerank(&rig, "tail", &candidates(&refs)).await;
    let expected: Vec<u32> = (10..20).chain(20..25).chain(0..10).collect();
    assert_eq!(
        out.order, expected,
        "scored 2 first, the unscored tail (ambient 1) next in input order, scored 0 last"
    );
    assert_eq!(
        out.note.as_deref(),
        Some("judged: re-ranked by relevance; 20 of 25 candidates rescored")
    );
    let rows = finish(rig).await?;
    assert_eq!(rows_of(&rows, "search-rank")[0].questions, 20);
    Ok(())
}

#[tokio::test]
async fn a_repeated_path_asks_one_question() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    // Four distinct paths in five entries: four answers. Five questions
    // against four answers is a parse error and the order would not change.
    let rig = rig_with_open_judge(cfg, vec![scores(&[0, 3, 1, 1])]).await?;
    let list = candidates(&["a.rs", "b.rs", "a.rs", "c.rs", "d.rs"]);
    let out = rerank(&rig, "dup", &list).await;
    assert_eq!(
        out.order,
        [1, 3, 4, 0, 2],
        "b, c, d, then both entries of a.rs, which share one memo score of 0"
    );
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "search-rank"), ["ok"]);
    assert_eq!(rows_of(&rows, "search-rank")[0].questions, 4);
    Ok(())
}

#[tokio::test]
async fn candidates_with_no_display_line_are_not_judged() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    // No judge step queued: any call would fail and write a provider row.
    let rig = rig_with_open_judge(cfg, Vec::new()).await?;
    let list: Vec<RerankCandidate> = ["a", "b", "c", "d"]
        .iter()
        .map(|path| RerankCandidate {
            path: (*path).into(),
            display: "  ".into(),
        })
        .collect();
    let out = rerank(&rig, "blank", &list).await;
    assert_eq!(out.order, [0, 1, 2, 3]);
    assert_eq!(out.note, None);
    assert!(finish(rig).await?.is_empty(), "no call, no row");
    Ok(())
}

#[tokio::test]
async fn two_or_fewer_candidates_are_never_judged() -> TestResult {
    let cfg = JudgedConfig {
        ranking: true,
        ..OFF
    };
    let rig = rig_with_open_judge(cfg, Vec::new()).await?;
    let out = rerank(&rig, "pair", &candidates(&["a", "b"])).await;
    assert_eq!(out.order, [0, 1]);
    assert_eq!(out.note, None);
    assert!(finish(rig).await?.is_empty());
    Ok(())
}

// ---- anchored asks ----------------------------------------------------------

fn anchor_cfg() -> JudgedConfig {
    JudgedConfig {
        ask_anchor: true,
        ..OFF
    }
}

fn is_blocked(turn: &Turn) -> bool {
    turn.outcome_text().starts_with("blocked by judged")
}

#[tokio::test]
async fn owner_decision_reaches_the_broker_with_one_row() -> TestResult {
    let script = vec![
        tool_step("ask", ASK_ONE),
        choice(OWNER_DECISION),
        text_step("final"),
    ];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    let turn = run_turn(&mut rig, "ask me").await?;
    assert!(
        !is_blocked(&turn),
        "owner-decision allows: {:?}",
        turn.outcomes
    );
    assert!(turn.last().ran, "the allowed ask ran");
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "ask-anchor"), ["ok"]);
    Ok(())
}

#[tokio::test]
async fn ideal_state_blocks_with_the_exact_reason_and_the_tool_never_runs() -> TestResult {
    let script = vec![
        tool_step("ask", ASK_ONE),
        choice(IDEAL_STATE),
        text_step("final"),
    ];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    let turn = run_turn(&mut rig, "ask me").await?;
    assert_eq!(
        turn.outcome_text(),
        format!("blocked by judged: {BLOCK_IDEAL_STATE}")
    );
    assert!(
        !turn.last().ran,
        "the model sees the block, the tool never runs"
    );
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "ask-anchor"), ["ok"]);
    Ok(())
}

#[tokio::test]
async fn a_multi_question_ask_is_one_judge_call() -> TestResult {
    let script = vec![
        tool_step("ask", ASK_THREE),
        choice(EXPLORE_FIRST),
        text_step("final"),
    ];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    let turn = run_turn(&mut rig, "ask me three things").await?;
    assert_eq!(
        turn.outcome_text(),
        format!("blocked by judged: {BLOCK_EXPLORE_FIRST}")
    );
    let rows = finish(rig).await?;
    assert_eq!(rows.len(), 1, "one call for three questions: {rows:?}");
    assert_eq!(statuses(&rows, "ask-anchor"), ["ok"]);
    Ok(())
}

#[tokio::test]
async fn an_explicit_ask_is_not_judged() -> TestResult {
    // No judge step: a judged call would eat the next turn step.
    let script = vec![tool_step("ask", ASK_EXPLICIT), text_step("final")];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    let turn = run_turn(&mut rig, "ask me").await?;
    assert!(
        !is_blocked(&turn),
        "explicit asks are never anchored: {:?}",
        turn.outcomes
    );
    assert!(
        !turn.last().text.starts_with("ask: "),
        "the ask tool accepted explicit:true: {:?}",
        turn.outcomes
    );
    assert_eq!(
        assistant_texts(&rig)?.last().map(String::as_str),
        Some("final")
    );
    assert!(
        finish(rig).await?.is_empty(),
        "no judge row for an explicit ask"
    );
    Ok(())
}

#[tokio::test]
async fn an_anchored_ask_never_fires_in_a_child_session() -> TestResult {
    let script = vec![
        text_step("parent"),
        tool_step("ask", ASK_ONE),
        choice(IDEAL_STATE),
        text_step("final"),
    ];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    run_turn(&mut rig, "open the parent").await?;
    let child = rig
        .host
        .open(
            SessionRef::Child {
                parent: rig.agent.session(),
                call: CallId::new("child-ask"),
                workspace: rig.workspace.clone(),
                name: None,
            },
            ClientId::new("judged-matrix-child"),
        )
        .await?;
    let mut subscription = child.subscribe(None)?;
    let turn = drive_turn(&child, &mut subscription, "ask me").await?;
    assert!(
        !is_blocked(&turn),
        "children are never anchored: {:?}",
        turn.outcomes
    );
    let child_session = child.session();
    drop(subscription);
    drop(child);
    rig.host.close(child_session).await?;
    assert!(
        finish(rig).await?.is_empty(),
        "the parent session ran no judge call"
    );
    Ok(())
}

#[tokio::test]
async fn a_judge_failure_allows_the_ask_with_one_row() -> TestResult {
    let script = vec![tool_step("ask", ASK_ONE), garbage(), text_step("final")];
    let mut rig = start(Setup::new(script, anchor_cfg()).with_ask()).await?;
    let turn = run_turn(&mut rig, "ask me").await?;
    assert!(
        !is_blocked(&turn),
        "any judge failure allows: {:?}",
        turn.outcomes
    );
    assert!(turn.last().ran, "the allowed ask ran");
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "ask-anchor"), ["parse"]);
    Ok(())
}

#[tokio::test]
async fn a_judge_timeout_allows_the_ask_with_one_timeout_row() -> TestResult {
    let script = vec![tool_step("ask", ASK_ONE), text_step("final")];
    let setup = Setup::new(script, anchor_cfg())
        .with_ask()
        .judge("[judge]\ngate = \"on\"\nmodel = \"acme/slow\"\ntimeout_ms = 1000\n");
    let mut rig = start(setup).await?;
    let turn = run_turn(&mut rig, "ask me").await?;
    assert!(!is_blocked(&turn), "a timeout allows: {:?}", turn.outcomes);
    assert!(turn.last().ran, "the allowed ask ran");
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "ask-anchor"), ["timeout"]);
    Ok(())
}

// ---- claim verification -----------------------------------------------------

fn claim_cfg() -> JudgedConfig {
    JudgedConfig {
        claim_check: true,
        dedup: true,
        ..OFF
    }
}

fn claim_reminders(rig: &Rig) -> TestResult<Vec<String>> {
    Ok(reminders(rig)?
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| text == CLAIM_REMINDER)
        .collect())
}

#[tokio::test]
async fn a_flagged_reply_delivers_the_exact_reminder_once_on_the_next_turn() -> TestResult {
    let script = vec![
        text_step(&reply_of(2_000, "R1")),
        verdict(true),
        text_step("short"),
        text_step("shorter"),
    ];
    let mut rig = start(Setup::new(script, claim_cfg())).await?;
    run_turn(&mut rig, "first").await?;
    assert!(
        claim_reminders(&rig)?.is_empty(),
        "nothing is delivered during the flagged turn"
    );
    run_turn(&mut rig, "second").await?;
    assert_eq!(
        claim_reminders(&rig)?,
        [CLAIM_REMINDER],
        "the next turn's context carries the exact reminder"
    );
    run_turn(&mut rig, "third").await?;
    assert_eq!(
        claim_reminders(&rig)?.len(),
        1,
        "delivered once, never again"
    );
    let rows = finish(rig).await?;
    assert_eq!(statuses(&rows, "claim-check"), ["ok"]);
    assert_eq!(rows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_reply_under_512_bytes_is_not_judged() -> TestResult {
    let script = vec![text_step(&reply_of(511, "R")), text_step("next")];
    let mut rig = start(Setup::new(script, claim_cfg())).await?;
    run_turn(&mut rig, "first").await?;
    run_turn(&mut rig, "second").await?;
    assert!(
        claim_reminders(&rig)?.is_empty(),
        "no reminder for a 511-byte reply"
    );
    assert!(finish(rig).await?.is_empty(), "no claim row for 511 bytes");
    Ok(())
}

#[tokio::test]
async fn a_reply_of_exactly_512_bytes_is_judged() -> TestResult {
    let script = vec![text_step(&reply_of(512, "R")), verdict(false)];
    let mut rig = start(Setup::new(script, claim_cfg())).await?;
    run_turn(&mut rig, "first").await?;
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "claim-check"),
        ["ok"],
        "the floor is inclusive"
    );
    Ok(())
}

#[tokio::test]
async fn the_claim_check_sees_the_whole_turn_across_tool_rounds() -> TestResult {
    // Two requests of 300 bytes each: neither reaches the 512-byte floor
    // alone, the turn's 600 bytes do.
    let first = format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":{}}},{{"type":"tool_call_started","id":"call-1","name":"no_such_tool"}},{{"type":"tool_calls_done","calls":[{{"id":"call-1","name":"no_such_tool","args":{{"kind":"parsed","value":{{}}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#,
        json_string(&reply_of(300, "A"))
    );
    let script = vec![first, text_step(&reply_of(300, "B")), verdict(false)];
    let mut rig = start(Setup::new(script, claim_cfg())).await?;
    run_turn(&mut rig, "work").await?;
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "claim-check"),
        ["ok"],
        "the settled turn's assistant text spans every round"
    );
    Ok(())
}

#[tokio::test]
async fn a_repeated_flag_is_refused_by_the_prefilter_with_no_extra_call() -> TestResult {
    let long = |marker: &str| text_step(&reply_of(1_000, marker));
    let script = vec![
        long("R1"),
        verdict(true),
        long("R2"),
        verdict(true),
        text_step("plain"),
    ];
    let mut rig = start(Setup::new(script, claim_cfg())).await?;
    run_turn(&mut rig, "one").await?;
    run_turn(&mut rig, "two").await?;
    assert_eq!(
        claim_reminders(&rig)?.len(),
        1,
        "the first flag is delivered at turn two"
    );
    run_turn(&mut rig, "three").await?;
    assert_eq!(
        claim_reminders(&rig)?.len(),
        1,
        "the identical second reminder is refused: the model never sees it twice"
    );
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "claim-check"),
        ["ok", "ok"],
        "two claim rows"
    );
    assert!(
        rows_of(&rows, "prompt-dedup").is_empty(),
        "a prefilter refusal is not a call: {rows:?}"
    );
    Ok(())
}

// ---- judged dedup -----------------------------------------------------------

#[tokio::test]
async fn a_borderline_candidate_is_judged_and_the_fifth_call_in_a_turn_admits_unjudged()
-> TestResult {
    use dalgona_batteries::judged::Admission::{Admit, Refuse};
    let cfg = JudgedConfig {
        dedup: true,
        claim_check: true,
        ..OFF
    };
    // Turn one is flagged (judge true), turn two delivers and records the
    // reminder; four borderline consults follow on one turn id.
    let script = vec![
        text_step(&reply_of(1_000, "R")),
        verdict(true),
        text_step("two"),
        verdict(true),
        verdict(false),
        verdict(true),
        verdict(false),
    ];
    let mut rig = start(Setup::new(script, cfg)).await?;
    run_turn(&mut rig, "one").await?;
    run_turn(&mut rig, "two").await?;
    assert_eq!(claim_reminders(&rig)?.len(), 1);
    let battery = Arc::clone(rig.battery.as_ref().expect("the battery runs"));
    let (session, _) = rig.probe.handle();
    let judge = battery
        .judge_session(session)
        .expect("the session judge is open");
    let borderline = "Your previous reply stated facts with confidence. Treat them as claims, \
                      not evidence: but please also double check every file path you mention";
    let turn = None;
    let mut decisions = Vec::new();
    for _ in 0..5 {
        decisions.push(
            battery
                .admit(session, "claim-check", borderline, turn, &judge)
                .await,
        );
    }
    assert_eq!(
        decisions,
        [Refuse, Admit, Refuse, Admit, Admit],
        "true refuses, false admits; the fifth consult in one turn admits with no call"
    );
    let rows = finish(rig).await?;
    assert_eq!(
        statuses(&rows, "prompt-dedup"),
        ["ok", "ok", "ok", "ok"],
        "exactly four dedup calls: {rows:?}"
    );
    Ok(())
}

// ---- every feature once -----------------------------------------------------

#[tokio::test]
async fn every_feature_writes_its_row_without_grants_or_cascade() -> TestResult {
    let cfg = JudgedConfig::default();
    let user_marker = "USER-MARKER-5c1f";
    let reply_marker = "REPLY-MARKER-0e77";
    let script = vec![
        // Turn one: classify, anchored ask (blocked), then a long flagged reply.
        choice(HIGH),
        tool_step("ask", ASK_ONE),
        choice(EXPLORE_FIRST),
        text_step(&reply_of(1_500, reply_marker)),
        verdict(true),
        // Turn two: classify, the reminder is delivered and recorded.
        choice(MEDIUM),
        text_step("done"),
        // Direct calls: a rerank batch and one borderline dedup consult.
        scores(&[0, 3, 1]),
        verdict(false),
    ];
    let mut rig = start(Setup::new(script, cfg).with_ask()).await?;
    let mut requests = 0;
    requests += run_turn(&mut rig, &format!("investigate {user_marker}"))
        .await?
        .requests_opened;
    requests += run_turn(&mut rig, "continue").await?.requests_opened;
    let ranked = rerank(&rig, "needle", &candidates(&["a", "b", "c"])).await;
    assert_eq!(ranked.order, [1, 2, 0]);
    let battery = Arc::clone(rig.battery.as_ref().expect("the battery runs"));
    let (session, _) = rig.probe.handle();
    let judge = battery
        .judge_session(session)
        .expect("the session judge is open");
    let borderline = "Your previous reply stated facts with confidence. Treat them as claims: \
                      verify the paths too, and say plainly what stays unverified afterwards";
    let _ = battery
        .admit(session, "claim-check", borderline, None, &judge)
        .await;
    assert_eq!(
        requests, 0,
        "every feature active: the broker opens no request"
    );
    let rows = finish(rig).await?;
    let mut features: Vec<&str> = rows.iter().map(|row| row.feature.as_str()).collect();
    features.sort_unstable();
    features.dedup();
    assert_eq!(
        features,
        [
            "ask-anchor",
            "claim-check",
            "prompt-dedup",
            "search-rank",
            "thinking"
        ],
        "exactly the five feature values: {rows:?}"
    );
    assert_eq!(
        statuses(&rows, "thinking"),
        ["ok", "ok"],
        "one classification per turn and no judge call classifies itself"
    );
    for row in &rows {
        assert!(row.raw.len() <= 1024, "row over the cap: {}", row.raw);
        assert!(
            !row.raw.contains(user_marker) && !row.raw.contains(reply_marker),
            "a row shares bytes with a judged payload: {}",
            row.raw
        );
    }
    Ok(())
}

// ---- toggles off ------------------------------------------------------------

#[tokio::test]
async fn with_every_toggle_off_the_judged_battery_adds_no_row() -> TestResult {
    let script = vec![text_step(&reply_of(1_000, "R")), text_step("next")];
    let mut rig = start(Setup::new(script, OFF)).await?;
    run_turn(&mut rig, "one").await?;
    run_turn(&mut rig, "two").await?;
    assert!(finish(rig).await?.is_empty());
    Ok(())
}
