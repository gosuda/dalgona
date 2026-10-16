//! Exercises Starlark TTSR rules against replayed model streams.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

#[path = "loop-plugin-06/catalog_endpoint.rs"]
mod catalog_endpoint;

use std::{
    error::Error,
    fmt::Write as _,
    fs,
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use catalog_endpoint::CatalogEndpoint;
use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord,
};
use dal_agent::{Delivery, Env, SessionRef, Subscription};
use dal_core::{
    CancelScope, Caps, Command, Config, ConfigProduct, EntryKind, Expect, ModelId, ModelRequest,
    Part, Product, Record, Reply, ServiceSet, SessionId, Stop, ThinkingLevel, UpdateKind, View,
    Workspace,
};
use dal_store::Store;
use support::{GateHarness, TestDir, scripted_session};
use tokio::sync::oneshot;

async fn scripted_ttsr_session(
    data: &TestDir,
    workspace: &TestDir,
    max_retries: usize,
) -> Result<(GateHarness, Subscription), Box<dyn Error + Send + Sync>> {
    let plugin_dir = data.path().join("plugins/gate-ttsr");
    fs::create_dir_all(&plugin_dir)?;
    let mut plugin = String::from(
        "load(\"@dal/v1\", \"dal\")\nplugin = dal.plugin(name = \"gate-ttsr\", version = \"0.1.0\", rules = {\n",
    );
    for attempt in 0..=max_retries {
        writeln!(
            plugin,
            "\"retry-control-{attempt}\": dal.rule(pattern = \"TRIPWIRE:{attempt}:\", text = \"Respect the gate rule.\", interrupt_mode = \"always\"),"
        )?;
    }
    plugin.push_str("})\n");
    fs::write(plugin_dir.join("plugin.star"), plugin)?;
    let replay_fixture = data.path().join("ttsr-scripted.jsonl");
    let mut replay = String::new();
    for attempt in 0..max_retries {
        replay.push_str(&RETRY_STEP.replace("TRIPWIRE", &format!("TRIPWIRE:{attempt}:")));
    }
    replay.push_str(&FINAL_STEP.replace("TRIPWIRE", &format!("TRIPWIRE:{max_retries}:")));
    fs::write(&replay_fixture, replay)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai/gpt-6\"\nplugins = [\"gate-ttsr\"]\n[rules]\njudge = \"off\"\nmax_retries = {max_retries}\n[providers.scripted]\nfixture = {:?}\n",
        replay_fixture.to_string_lossy()
    );
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::Ephemeral {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
    };
    let harness = scripted_session(product, config, env, session).await?;
    let subscription = harness.agent.subscribe(None)?;
    Ok((harness, subscription))
}

fn assert_replay_view(view: &View, max_retries: usize) {
    let reminders: Vec<_> = view
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::Reminder { source, text } => Some((source.as_ref(), text.as_ref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        reminders.len(),
        max_retries + 1,
        "the post-cap rule match is still persisted"
    );
    for (attempt, (source, text)) in reminders[..max_retries].iter().enumerate() {
        assert_eq!(*source, format!("rule:retry-control-{attempt}"));
        assert!(text.contains("<system-interrupt"));
        assert!(text.contains("Respect the gate rule."));
    }
    let (source, reminder) = reminders[max_retries];
    assert_eq!(source, format!("rule:retry-control-{max_retries}"));
    assert!(reminder.contains("<system-reminder"));
    assert!(reminder.contains("Respect the gate rule."));
    let assistant_text = view
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
        .collect::<String>();
    assert_eq!(
        assistant_text,
        format!("final answer after the retry cap: TRIPWIRE:{max_retries}:")
    );
    assert!(!assistant_text.contains("partial attempt"));
}

const RETRY_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"partial attempt TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const FINAL_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final answer after the retry cap: TRIPWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

#[tokio::test]
async fn production_rules_honor_the_configured_retry_limit()
-> Result<(), Box<dyn Error + Send + Sync>> {
    for max_retries in [0, 3, 4, 20] {
        let data = TestDir::new()?;
        let workspace = TestDir::new()?;
        let (harness, mut subscription) =
            scripted_ttsr_session(&data, &workspace, max_retries).await?;
        let accepted = harness
            .agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "Return the scripted response.".into(),
                }],
            })
            .await?;
        assert!(matches!(accepted, Reply::Accepted { .. }));
        let mut rule_updates = 0;
        let mut ended = false;
        while !ended {
            let delivery =
                tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
            let Some(delivery) = delivery else { break };
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RuleFired { rule, .. } => {
                    assert_eq!(rule.as_ref(), format!("retry-control-{rule_updates}"));
                    rule_updates += 1;
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn, "retry limit {max_retries}");
                    ended = true;
                }
                UpdateKind::Notice(notice)
                    if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
                {
                    panic!("retry limit {max_retries}: {}", notice.text);
                }
                _ => {}
            }
        }
        assert!(
            ended,
            "the response must complete with retry limit {max_retries}"
        );
        assert_eq!(
            rule_updates,
            max_retries + 1,
            "each delivered reminder must fire with retry limit {max_retries}",
        );
        let view = harness.agent.view(dal_core::PageReq::default())?;
        assert_replay_view(&view, max_retries);
        let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    }
    Ok(())
}

const TURN_LIMIT: Duration = Duration::from_secs(10);

const GATE_RULE_PLUGIN: &str = r#"load("@dal/v1", "dal")

ladder = dal.rule(
    pattern = "TRIPWIRE",
    text = "Respect the gate rule.",
    interrupt_mode = "always",
)

cap = dal.rule(
    pattern = "CAPWIRE",
    text = "Respect the post-cap gate rule.",
    interrupt_mode = "always",
)

verdict_gate = dal.rule(
    pattern = "VERDICTWIRE",
    text = "Settle the verdict gate.",
    judge = "Does this reply violate the verdict gate?",
    interrupt_mode = "always",
)

gated = dal.rule(
    pattern = "PROBEWIRE",
    text = "The judged probe stays inert without a ready judge.",
    judge = "Did the reply probe the provider?",
    interrupt_mode = "always",
)

plugin = dal.plugin(
    name = "gate-ttsr",
    version = "0.1.0",
    rules = {"ladder": ladder, "cap": cap, "verdict-gate": verdict_gate, "gated": gated},
)
"#;

const VERDICT_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"partial attempt VERDICTWIRE\"},{\"type\":\"text_delta\",\"text\":\" tail after the interrupt\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const JUDGE_TRUE_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"{\\\"answers\\\":[true]}\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const JUDGE_FALSE_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"{\\\"answers\\\":[false]}\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const JUDGE_GARBAGE_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"not a verdict\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const CLEAN_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final verdict-gated answer\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const CLEAN_FALSE_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final retry without the judged fire\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const CLEAN_RETRY_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final answer after the judge retry\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const FOLLOW_UP_TEXT: &str = "follow-up marker after the judged failure";
const FOLLOW_UP_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"follow-up marker after the judged failure\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";
const FINAL_PROBE_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"final gate-off answer TRIPWIRE CAPWIRE PROBEWIRE\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// What one turn delivered to the subscriber before its terminal.
struct TurnDelivery {
    rule_fires: Vec<Box<str>>,
    notices: Vec<dal_core::Notice>,
    delta_text: String,
    stop: Option<Stop>,
}

/// Collects one turn; timeout, closure, and driver failure remain errors.
async fn await_turn_end(
    subscription: &mut Subscription,
    label: &str,
) -> Result<TurnDelivery, Box<dyn Error + Send + Sync>> {
    let mut delivery = TurnDelivery {
        rule_fires: Vec::new(),
        notices: Vec::new(),
        delta_text: String::new(),
        stop: None,
    };
    loop {
        let update = tokio::time::timeout(TURN_LIMIT, subscription.next())
            .await
            .map_err(|_| format!("{label}: the turn did not end in time"))?
            .ok_or_else(|| format!("{label}: the subscription closed before the turn ended"))?;
        let Delivery::Update(update) = update else {
            continue;
        };
        match &update.kind {
            UpdateKind::RuleFired { rule, .. } => delivery.rule_fires.push(rule.clone()),
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("{label}: {}", notice.text).into());
            }
            UpdateKind::Notice(notice) => delivery.notices.push(notice.clone()),
            UpdateKind::TurnEnded { stop, .. } => {
                delivery.stop = Some(*stop);
                return Ok(delivery);
            }
            UpdateKind::Delta { text, .. } => delivery.delta_text.push_str(text),
            _ => {}
        }
    }
}

type Reminders = Vec<(Box<str>, Box<str>)>;

/// Every reminder source and assistant text in journal order.
fn journal_texts(view: &View) -> (Reminders, String) {
    let mut reminders = Vec::new();
    let mut assistant = String::new();
    for entry in &view.entries.items {
        match &entry.kind {
            EntryKind::Reminder { source, text } => {
                reminders.push((source.clone(), text.clone()));
            }
            EntryKind::Assistant { content, .. } => {
                for block in content {
                    if let dal_core::Block::Text { text, .. } = block {
                        assistant.push_str(text);
                    }
                }
            }
            _ => {}
        }
    }
    (reminders, assistant)
}

fn reminder_sources(delivery: &View) -> Vec<String> {
    journal_texts(delivery)
        .0
        .iter()
        .map(|(source, _)| source.to_string())
        .collect()
}
/// Counts durable extension records after the host has released its journal.
async fn durable_ext_records(
    data_root: &Path,
    workspace: Workspace,
    session: SessionId,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let store = Store::new(data_root.to_path_buf(), workspace, Product::Dal);
    let (journal, _) = store.open_session(session).await?;
    Ok(journal
        .records()
        .iter()
        .filter(
            |record| matches!(record, Record::Ext { kind, .. } if kind.as_ref() == "rule_fired"),
        )
        .count())
}

fn write_fixture(
    data: &TestDir,
    name: &str,
    steps: &[&str],
) -> Result<std::path::PathBuf, Box<dyn Error + Send + Sync>> {
    let fixture = data.path().join(name);
    fs::write(&fixture, steps.concat())?;
    Ok(fixture)
}

fn gate_user(
    judge_gate: &str,
    rules_judge: &str,
    max_retries: usize,
    fixture: &std::path::Path,
) -> String {
    format!(
        "model = \"openai/gpt-6\"\nplugins = [\"gate-ttsr\"]\n[rules]\njudge = {rules_judge:?}\nmax_retries = {max_retries}\n[judge]\ngate = {judge_gate:?}\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    )
}

/// Builds the dalgon product host with the shared gate plugin installed.
async fn start_gate_host(
    data: &TestDir,
    workspace: &TestDir,
    user: String,
) -> Result<dal_agent::Host, Box<dyn Error + Send + Sync>> {
    let plugin_dir = data.path().join("plugins/gate-ttsr");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), GATE_RULE_PLUGIN)?;
    let factory = dalgon::product();
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, config, env).await?;
    Ok(host)
}

async fn open_gate_agent(
    host: &dal_agent::Host,
    workspace: &TestDir,
) -> Result<dal_agent::Agent, Box<dyn Error + Send + Sync>> {
    Ok(host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
            },
            dal_core::ClientId::new("gate-ttsr"),
        )
        .await?)
}

async fn open_persistent_gate_agent(
    host: &dal_agent::Host,
    workspace: &TestDir,
) -> Result<dal_agent::Agent, Box<dyn Error + Send + Sync>> {
    Ok(host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.path().to_path_buf())?,
                name: None,
            },
            dal_core::ClientId::new("gate-ttsr-persistent"),
        )
        .await?)
}

async fn durable_gate_session(
    data: &TestDir,
    workspace: &TestDir,
    user: String,
) -> Result<(GateHarness, Subscription), Box<dyn Error + Send + Sync>> {
    let host = start_gate_host(data, workspace, user).await?;
    let agent = open_persistent_gate_agent(&host, workspace).await?;
    let subscription = agent.subscribe(None)?;
    Ok((GateHarness { host, agent }, subscription))
}

async fn gate_session(
    data: &TestDir,
    workspace: &TestDir,
    user: String,
) -> Result<(GateHarness, Subscription), Box<dyn Error + Send + Sync>> {
    let host = start_gate_host(data, workspace, user).await?;
    let agent = open_gate_agent(&host, workspace).await?;
    let subscription = agent.subscribe(None)?;
    Ok((GateHarness { host, agent }, subscription))
}

/// Asserts the turn stopped normally and reports its rule fires.
fn assert_ended(delivery: &TurnDelivery, label: &str) {
    assert_eq!(
        delivery.stop,
        Some(Stop::EndTurn),
        "{label}: the turn must complete normally"
    );
}

/// The not-ready startup composition: every provider catalog resolves
/// against one loopback endpoint, and the judge model has no credentials.
fn unready_user(endpoint: &CatalogEndpoint, judge_gate: &str) -> String {
    let base = endpoint.base_url();
    format!(
        "model = \"gatelocal/plain\"\nplugins = [\"gate-ttsr\"]\n[rules]\njudge = \"auto\"\n[judge]\ngate = {judge_gate:?}\n[providers.openai]\nbase_url = \"{base}\"\n[providers.anthropic]\nbase_url = \"{base}\"\n[providers.openai-codex]\nbase_url = \"{base}\"\n[providers.gatelocal]\napi = \"openai_chat\"\nbase_url = \"{base}\"\nkey_env = \"GATELOCAL_KEY\"\n"
    )
}

/// The judge model id served by the test's own native extension.
const JUDGE_MODEL: &str = "kit/tardygavel";
/// The judge timeout the deadline cell drives through paused virtual time.
const JUDGE_TIMEOUT_MS: u64 = 1_000;
/// The judge timeout the cancellation cell keeps far beyond its budget.
const CANCEL_JUDGE_TIMEOUT_MS: u64 = 300_000;

/// The always-tardy judge: every call announces its entry once, then waits
/// forever, so the test drives the product's own timeout and cancellation
/// paths instead of scripting the verdict.
#[derive(Clone)]
struct TardyJudge {
    entered: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    calls: Arc<AtomicUsize>,
}

impl TardyJudge {
    fn armed() -> (Self, oneshot::Receiver<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        (
            Self {
                entered: Arc::new(Mutex::new(Some(entered_tx))),
                calls: Arc::new(AtomicUsize::new(0)),
            },
            entered_rx,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<oneshot::Sender<()>>> {
        self.entered.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ModelHandler for TardyJudge {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = self.lock().take() {
            let _ = entered.send(());
        }
        Box::pin(std::future::pending::<Result<EventStream, ModelError>>())
    }
}

/// Registers the tardy judge as a native model beside the product's own
/// extensions; readiness resolves it without opening a stream.
fn tardy_judge_extension(judge: TardyJudge) -> Result<Extension, Box<dyn Error + Send + Sync>> {
    let record = ModelRecord {
        id: ModelId::parse(JUDGE_MODEL)?,
        caps: Caps {
            context_window: Some(100_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(judge),
        export: None,
    };
    ExtensionBuilder::new("judge-tardiness", "0.1.0", ServiceSet::EMPTY)?
        .model(record)
        .build()
        .map_err(Into::into)
}

/// Builds the dalgon product host with the gate plugin and the tardy judge
/// model appended to the product's own extensions.
async fn timed_gate_host(
    data: &TestDir,
    workspace: &TestDir,
    user: String,
    judge: TardyJudge,
) -> Result<dal_agent::Host, Box<dyn Error + Send + Sync>> {
    let plugin_dir = data.path().join("plugins/gate-ttsr");
    fs::create_dir_all(&plugin_dir)?;
    fs::write(plugin_dir.join("plugin.star"), GATE_RULE_PLUGIN)?;
    let factory = dalgon::product();
    let config = Config::load(
        ConfigProduct::Dalgon,
        data.path(),
        factory.defaults,
        Some(&user),
    )?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(tardy_judge_extension(judge)?);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let host = dal_agent::Host::start(product, config, env).await?;
    Ok(host)
}

/// The gate-on composition with the judge pinned to the tardy native model.
fn timed_judge_user(timeout_ms: u64, fixture: &std::path::Path) -> String {
    format!(
        "model = \"openai/gpt-6\"\nplugins = [\"gate-ttsr\"]\n[rules]\njudge = \"on\"\nmax_retries = 3\n[judge]\ngate = \"on\"\nmodel = \"{JUDGE_MODEL}\"\ntimeout_ms = {timeout_ms}\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    )
}

async fn await_judge_entry(
    entry: oneshot::Receiver<()>,
    label: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    tokio::time::timeout(TURN_LIMIT, entry)
        .await
        .map_err(|_| format!("{label}: the judge did not enter in time"))?
        .map_err(|_| format!("{label}: the judge entry signal was dropped"))?;
    Ok(())
}

/// Gate off must not probe the provider at startup: the first scripted step
/// belongs to the first user turn, and judged rules stay inert.
///
/// Catches: a startup readiness probe consuming a scripted step (the judge
/// startup defect), and judged rules leaking into the set while the gate is
/// off.
#[tokio::test]
async fn judge_gate_off_never_probes_and_keeps_judged_rules_inert()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = write_fixture(&data, "gate-off.jsonl", &[RETRY_STEP, FINAL_PROBE_STEP])?;
    let (harness, mut subscription) =
        gate_session(&data, &workspace, gate_user("off", "auto", 1, &fixture)).await?;
    let _ = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted response.".into(),
            }],
        })
        .await?;
    let delivery = await_turn_end(&mut subscription, "gate off").await?;
    assert_ended(&delivery, "gate off");
    let fired: Vec<_> = delivery.rule_fires.iter().map(AsRef::as_ref).collect();
    assert_eq!(
        fired,
        ["ladder", "cap"],
        "the unjudged ladder owns every fire; a startup probe would shift the fixture"
    );
    let view = harness.agent.view(dal_core::PageReq::default())?;
    assert_eq!(
        reminder_sources(&view),
        ["rule:ladder", "rule:cap"],
        "the judged rule never delivers while the gate is off"
    );
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant, "final gate-off answer TRIPWIRE CAPWIRE PROBEWIRE",
        "the old rule stays latched, the new rule reminds, and the judged rule stays inert"
    );
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

/// A ready judge under gate auto and on delivers the true verdict through
/// the interrupt path: reminder, rule fire, and a discarded stream tail.
///
/// Catches: judging before the provider stream is released (the queued
/// verdict would become the retry answer), delivering the interrupted
/// response, and skipping the judged lane entirely.
#[tokio::test]
async fn judge_ready_gate_delivers_a_true_verdict_as_reminder_retry()
-> Result<(), Box<dyn Error + Send + Sync>> {
    for judge_gate in ["auto", "on"] {
        let data = TestDir::new()?;
        let workspace = TestDir::new()?;
        let fixture = write_fixture(
            &data,
            "gate-true.jsonl",
            &[VERDICT_STEP, JUDGE_TRUE_STEP, CLEAN_STEP],
        )?;
        let (harness, mut subscription) =
            gate_session(&data, &workspace, gate_user(judge_gate, "on", 3, &fixture)).await?;
        let _ = harness
            .agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "Return the scripted response.".into(),
                }],
            })
            .await?;
        let delivery = await_turn_end(&mut subscription, judge_gate).await?;
        assert_ended(&delivery, judge_gate);
        let fired: Vec<_> = delivery.rule_fires.iter().map(AsRef::as_ref).collect();
        assert_eq!(
            fired,
            ["verdict-gate"],
            "{judge_gate}: the accepted judged fire publishes exactly one rule fire"
        );
        assert!(
            !delivery.delta_text.contains("tail after the interrupt"),
            "{judge_gate}: events after the interrupt stay discarded"
        );
        assert!(
            delivery
                .notices
                .iter()
                .all(|notice| notice.kind.as_ref() != "rules"),
            "{judge_gate}: an accepted violation must not report that the rule passed"
        );
        let view = harness.agent.view(dal_core::PageReq::default())?;
        assert_eq!(
            reminder_sources(&view),
            ["rule:verdict-gate"],
            "{judge_gate}: the accepted verdict retries with one reminder"
        );
        let (_, assistant) = journal_texts(&view);
        assert_eq!(
            assistant, "final verdict-gated answer",
            "{judge_gate}: the retry consumes the step after the judged verdict, never the interrupted attempt"
        );
        let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    }
    Ok(())
}

/// A false verdict retries with one pass note but no reminder or record.
///
/// Catches: a rejected verdict delivered as a report record or reminder,
/// and the judged lane never consulted (the queued verdict would become the
/// retry answer and fail the final-text assertion).
#[tokio::test]
async fn judge_false_verdict_retries_without_reminder_or_record()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = write_fixture(
        &data,
        "gate-false.jsonl",
        &[VERDICT_STEP, JUDGE_FALSE_STEP, CLEAN_FALSE_STEP],
    )?;
    let (harness, mut subscription) =
        gate_session(&data, &workspace, gate_user("on", "on", 3, &fixture)).await?;
    let _ = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted response.".into(),
            }],
        })
        .await?;
    let delivery = await_turn_end(&mut subscription, "gate false").await?;
    assert_ended(&delivery, "gate false");
    assert!(
        delivery.rule_fires.is_empty(),
        "a false verdict must not publish a rule fire: {:?}",
        delivery.rule_fires
    );
    let rules_notes = delivery
        .notices
        .iter()
        .filter(|notice| notice.kind.as_ref() == "rules")
        .count();
    assert_eq!(
        rules_notes, 1,
        "a false verdict reports one informational pass note"
    );
    let view = harness.agent.view(dal_core::PageReq::default())?;
    assert!(
        reminder_sources(&view).is_empty(),
        "a false verdict retries without a reminder"
    );
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant, "final retry without the judged fire",
        "the retry consumes the step after the rejected verdict"
    );
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}

/// A judge failure retries the main response once, silently, without a
/// second judgment.
///
/// The rule latches before the judged wait, so a failed verdict must not be
/// judged again in the same turn: the driver retries the interrupted main
/// response, delivers no fire, no reminder, and no extension record, and
/// surfaces an informational failure note. The marker step proves the turn
/// made exactly three model calls (interrupted response, failed judgment,
/// retry): any second judge call would consume the marker step and leave
/// the follow-up prompt without an answer.
///
/// Catches: a judge error failing closed (turn failure), re-judging the
/// latched rule (the marker answer shifts and the follow-up starves), a
/// judged fire or reminder delivered from a failed verdict, and a retry
/// that replays the interrupted response instead of calling the model.
#[tokio::test]
async fn judge_error_retries_the_main_response_once_without_judging_again()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = write_fixture(
        &data,
        "gate-error.jsonl",
        &[
            VERDICT_STEP,
            JUDGE_GARBAGE_STEP,
            CLEAN_RETRY_STEP,
            FOLLOW_UP_STEP,
        ],
    )?;
    let (harness, mut subscription) =
        durable_gate_session(&data, &workspace, gate_user("on", "on", 3, &fixture)).await?;
    let _ = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted response.".into(),
            }],
        })
        .await?;
    let delivery = await_turn_end(&mut subscription, "gate error").await?;
    assert_ended(&delivery, "gate error");
    let rules_notes = delivery
        .notices
        .iter()
        .filter(|notice| notice.kind.as_ref() == "rules")
        .count();
    assert_eq!(
        rules_notes, 1,
        "the judge failure carries exactly one informational note"
    );
    assert!(
        delivery.rule_fires.is_empty(),
        "a failed judgment must not publish a rule fire: {:?}",
        delivery.rule_fires
    );
    assert!(
        !delivery.delta_text.contains("tail after the interrupt"),
        "the interrupted response stays discarded through the retry"
    );
    let view = harness.agent.view(dal_core::PageReq::default())?;
    let session_id = view.session.id;
    assert!(
        reminder_sources(&view).is_empty(),
        "a failed judgment retries without a reminder: {:?}",
        reminder_sources(&view)
    );
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant, "final answer after the judge retry",
        "the silent retry is a fresh main response, not the interrupted attempt"
    );
    let _ = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "The follow-up consumes the marker step.".into(),
            }],
        })
        .await?;
    let follow_up = await_turn_end(&mut subscription, "gate error follow-up").await?;
    assert_ended(&follow_up, "gate error follow-up");
    let view = harness.agent.view(dal_core::PageReq::default())?;
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant,
        format!("final answer after the judge retry{FOLLOW_UP_TEXT}"),
        "the follow-up answers with the marker step, proving the turn made exactly one judge call"
    );
    harness.host.close(session_id).await?;
    let workspace_root = Workspace::new(workspace.path().to_path_buf())?;
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    assert_eq!(
        durable_ext_records(data.path(), workspace_root, session_id).await?,
        0,
        "a failed judgment must not persist a rule_fired extension record"
    );
    Ok(())
}

/// Gate auto without a usable route opens the session, resolves judged
/// rules inert with exactly one readiness note at first inference, and the
/// credential-free inference still fails visibly.
///
/// Catches: auto failing session open (the fail-closed leak from gate on),
/// auto passing silently without its readiness note, and a swallowed
/// provider failure behind an unready gate.
#[tokio::test]
async fn judge_auto_gate_opens_with_a_readiness_note_when_no_route_is_ready()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let endpoint = CatalogEndpoint::start().await?;
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let (harness, mut subscription) =
        gate_session(&data, &workspace, unready_user(&endpoint, "auto")).await?;
    let _ = harness
        .agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "The unready gate still answers prompts with a visible failure.".into(),
            }],
        })
        .await?;
    let mut rules_notes = 0;
    let mut driver_failed = false;
    let stop = loop {
        let update = tokio::time::timeout(TURN_LIMIT, subscription.next())
            .await
            .unwrap_or_else(|_| panic!("gate auto: the turn did not end in time"));
        let Some(Delivery::Update(update)) = update else {
            continue;
        };
        match &update.kind {
            UpdateKind::Notice(notice) if notice.kind.as_ref() == "rules" => rules_notes += 1,
            UpdateKind::Notice(notice) if notice.kind.as_ref() == "driver.failed" => {
                driver_failed = true;
            }
            UpdateKind::TurnEnded { stop, .. } => break *stop,
            _ => {}
        }
    };
    assert_eq!(
        stop,
        Stop::Failed,
        "gate auto: the unready route fails the turn visibly"
    );
    assert!(
        !driver_failed,
        "gate auto: the session itself stays alive behind an unready route"
    );
    assert_eq!(
        rules_notes, 1,
        "gate auto: exactly one readiness note resolves the judge off"
    );
    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    endpoint.close().await?;
    Ok(())
}

/// Gate on without a usable route fails session open, where the paired auto
/// composition with the same providers opens cleanly.
///
/// Catches: gate on resolving to a silent off and publishing an active
/// session without a ready judge route.
#[tokio::test]
async fn judge_on_gate_fails_closed_without_a_ready_route()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let endpoint = CatalogEndpoint::start().await?;
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let host = start_gate_host(&data, &workspace, unready_user(&endpoint, "on")).await?;
    let opened = open_gate_agent(&host, &workspace).await;
    let Err(error) = opened else {
        let _ = host.shutdown(Duration::from_secs(1)).await;
        panic!("gate on must fail session open without a ready route");
    };
    let error = error
        .downcast_ref::<dal_agent::HostError>()
        .unwrap_or_else(|| panic!("session open failed with an unexpected error type: {error}"));
    assert!(
        matches!(error, dal_agent::HostError::Config { .. }),
        "the judge gate fails setup with a typed config error: {error:?}"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    endpoint.close().await?;
    Ok(())
}

/// A real judge deadline causes one silent main retry and no durable rule fire.
#[tokio::test]
async fn judge_timeout_retries_the_main_response_once_without_a_reminder()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = write_fixture(&data, "gate-timeout.jsonl", &[VERDICT_STEP, CLEAN_STEP])?;
    let (judge, entry) = TardyJudge::armed();
    let host = timed_gate_host(
        &data,
        &workspace,
        timed_judge_user(JUDGE_TIMEOUT_MS, &fixture),
        judge.clone(),
    )
    .await?;
    let agent = open_persistent_gate_agent(&host, &workspace).await?;
    let mut subscription = agent.subscribe(None)?;
    let _ = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Return the scripted response.".into(),
            }],
        })
        .await?;
    await_judge_entry(entry, "judge timeout").await?;
    tokio::time::pause();
    assert_eq!(
        judge.calls(),
        1,
        "the judged wait entered its first and only judge call before the deadline"
    );
    tokio::time::advance(Duration::from_millis(JUDGE_TIMEOUT_MS + 1)).await;
    tokio::time::resume();
    let delivery = await_turn_end(&mut subscription, "judge timeout").await?;
    assert_eq!(
        delivery.stop,
        Some(Stop::EndTurn),
        "the timeout retries silently instead of failing the turn"
    );
    let rules_notes = delivery
        .notices
        .iter()
        .filter(|notice| notice.kind.as_ref() == "rules")
        .count();
    assert_eq!(
        rules_notes, 1,
        "the timeout carries exactly one informational note"
    );
    assert!(
        delivery.rule_fires.is_empty(),
        "a timed-out judgment must not publish a rule fire: {:?}",
        delivery.rule_fires
    );
    assert!(
        !delivery.delta_text.contains("tail after the interrupt"),
        "the interrupted response stays discarded through the timeout retry"
    );
    let view = agent.view(dal_core::PageReq::default())?;
    let session_id = view.session.id;
    assert!(
        reminder_sources(&view).is_empty(),
        "a timed-out judgment retries without a reminder: {:?}",
        reminder_sources(&view)
    );
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant, "final verdict-gated answer",
        "the timeout retry consumes the next main-model step"
    );
    assert_eq!(
        judge.calls(),
        1,
        "the timed-out judgment is never judged again in the same turn"
    );
    let workspace_root = Workspace::new(workspace.path().to_path_buf())?;
    host.close(session_id).await?;
    let shutdown = host.shutdown(Duration::from_secs(1)).await;
    assert!(
        shutdown.status_quiet,
        "host status did not drain: {shutdown:?}"
    );
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "host tasks did not drain: {shutdown:?}"
    );
    assert_eq!(
        durable_ext_records(data.path(), workspace_root, session_id).await?,
        0,
        "a timed-out judgment must not persist a rule_fired extension record"
    );
    Ok(())
}

/// Cancelling the turn during a product judged wait delivers no late
/// writes, and the pending judge never answers afterwards.
///
/// The tardy judge enters and waits with its deadline far away, so the
/// cancellation alone resolves the wait. The follow-up prompt then consumes
/// the step the cancelled turn never took, proving no late retry ran.
///
/// Catches: a cancelled judged wait publishing a late rule fire, reminder,
/// or extension record, a retry response running after cancellation, and
/// a cancelled turn consuming the follow-up's scripted step.
#[tokio::test]
async fn cancel_during_the_product_judged_wait_delivers_no_late_writes()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = write_fixture(&data, "gate-cancel.jsonl", &[VERDICT_STEP, CLEAN_STEP])?;
    let (judge, entry) = TardyJudge::armed();
    let host = timed_gate_host(
        &data,
        &workspace,
        timed_judge_user(CANCEL_JUDGE_TIMEOUT_MS, &fixture),
        judge.clone(),
    )
    .await?;
    let agent = open_persistent_gate_agent(&host, &workspace).await?;
    let mut subscription = agent.subscribe(None)?;
    let accepted = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Cancel inside the product judged wait.".into(),
            }],
        })
        .await?;
    let Reply::Accepted { turn, .. } = accepted else {
        panic!("prompt not accepted: {accepted:?}");
    };
    await_judge_entry(entry, "judge cancel").await?;
    agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await?;
    let delivery = await_turn_end(&mut subscription, "judge cancel").await?;
    assert_eq!(
        delivery.stop,
        Some(Stop::Cancelled),
        "the cancelled turn ends cancelled before the judge deadline"
    );
    assert!(
        delivery.rule_fires.is_empty(),
        "a cancelled judged wait must not publish a rule fire: {:?}",
        delivery.rule_fires
    );
    assert!(
        !delivery.delta_text.contains("final verdict-gated answer"),
        "a cancelled turn must not run the retry response"
    );
    let view = agent.view(dal_core::PageReq::default())?;
    let session_id = view.session.id;
    assert!(
        reminder_sources(&view).is_empty(),
        "a cancelled judged wait leaves no reminder: {:?}",
        reminder_sources(&view)
    );
    let (_, assistant) = journal_texts(&view);
    assert!(
        assistant.is_empty(),
        "the cancelled attempt leaves no assistant record: {assistant:?}"
    );
    assert_eq!(
        judge.calls(),
        1,
        "the cancelled judged wait never judges again"
    );
    let _ = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "The follow-up owns the remaining step.".into(),
            }],
        })
        .await?;
    let follow_up = await_turn_end(&mut subscription, "judge cancel follow-up").await?;
    assert_eq!(
        follow_up.stop,
        Some(Stop::EndTurn),
        "the follow-up completes normally after the cancelled turn"
    );
    let view = agent.view(dal_core::PageReq::default())?;
    let (_, assistant) = journal_texts(&view);
    assert_eq!(
        assistant, "final verdict-gated answer",
        "the follow-up consumes the step the cancelled turn never took"
    );
    let workspace_root = Workspace::new(workspace.path().to_path_buf())?;
    host.close(session_id).await?;
    let shutdown = host.shutdown(Duration::from_secs(1)).await;
    assert!(
        shutdown.status_quiet,
        "host status did not drain: {shutdown:?}"
    );
    assert_eq!(
        shutdown.tasks_remaining, 0,
        "host tasks did not drain: {shutdown:?}"
    );
    assert_eq!(
        durable_ext_records(data.path(), workspace_root, session_id).await?,
        0,
        "a cancelled judged wait must not persist a rule_fired extension record"
    );
    Ok(())
}
