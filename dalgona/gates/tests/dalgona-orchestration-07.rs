// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the `[plugin.orchestration.isolation]`
//! key parses, an unknown sub-battery key still fails the build, and the
//! arbiter/inflight coherence refusal names its dep. The synthetic-model
//! criteria ride the same target: the scripted Starlark handler contract,
//! typed cycle and depth routing errors, and the relay-only router denials.
use dalgona_gates::support;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord, ToolOutput,
};
use dal_agent::{Delivery, Env, Host, SessionRef};
use dal_core::{
    AgentStart, Budget, CallId, Caps, ClientId, Command, ContextItem, Expect, Family, ModelId,
    ModelInfo, ModelRequest, ModelRoute, Name, OnError, Origin, PageReq, Part, Purpose, RawJson,
    Reply, RequestParams, ScopeSpec, ServiceSet, Stop, ThinkingLevel, ToolClass, ToolSpec,
    UpdateKind, Visibility, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent as ProviderEvent};

#[test]
fn isolation_key_parses_and_coherence_refusals_name_their_dep() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-isolation-key")?;

    // The isolation key is accepted and reaches the battery config.
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        scratch.path(),
        dalgona::product().defaults,
        Some("[plugin.orchestration.isolation]\nenabled = false\n"),
    )?;
    let cx = dalgon::BuildCx {
        data_root: scratch.path().to_path_buf(),
        config: &config,
    };
    let product = dalgona::build(&cx)?;
    let orchestration = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "orchestration")
        .ok_or("orchestration battery missing")?;
    assert!(
        orchestration
            .tools()
            .iter()
            .any(|(tool, _)| tool.name().as_str() == "agents"),
        "agents must stay registered with isolation off"
    );

    // An unknown sub-battery key is a startup error, not a silent default.
    let error = build_refusal(
        &scratch,
        "[plugin.orchestration.no_such_battery]\nenabled = true\n",
    )?;
    assert!(
        error.contains("no_such_battery"),
        "unknown-key error must name the key: {error}"
    );

    // Coherence: arbiter cannot be off while goal is on.
    let error = build_refusal(
        &scratch,
        "[plugin.orchestration.arbiter]\nenabled = false\n\n\
         [plugin.orchestration.goal]\nenabled = true\n",
    )?;
    assert!(
        error.contains(
            "orchestration: [plugin.orchestration.arbiter] cannot be off while goal is on."
        ),
        "coherence error text drifted: {error}"
    );

    // The built product still serves a live host with the battery intact.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        assert!(doc.text.contains("## Constants"));
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

/// Loads `toml` over the product defaults, which the generic loader accepts,
/// and returns the refusal the strict battery decode raises when the product
/// is built.
fn build_refusal(scratch: &support::Scratch, toml: &str) -> support::TestResult<String> {
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgona,
        scratch.path(),
        dalgona::product().defaults,
        Some(toml),
    )?;
    let cx = dalgon::BuildCx {
        data_root: scratch.path().to_path_buf(),
        config: &config,
    };
    match dalgona::build(&cx) {
        Ok(_) => Err(format!("the product built from {toml:?}").into()),
        Err(error) => Ok(error.to_string()),
    }
}

const NO_BATTERIES: &str = "disabled_batteries = [\"ask\", \"history\", \"mcp\", \"orchestration\", \
     \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n";
const TURN_WAIT: Duration = Duration::from_secs(60);

/// How one prompted turn ended, the session's rendered view, the typed
/// history entries behind it, the actual assistant text, notices seen before
/// the end, and the session id.
struct TurnOutcome {
    stop: Stop,
    view: String,
    entries: Vec<dal_core::EntryKind>,
    text: String,
    notices: String,
    session: String,
}

/// The actual assistant-visible text across the typed view, without `Debug` escaping.
///
/// The gate previously matched quoted chain evidence against
/// `format!("{view:?}")`, whose `Debug` escapes inner quotes (`\"`), so
/// raw-quote expectations like `Synthetic { id: "dalgona/cycle" }` never
/// match even when the assistant `Block::Text` carries them verbatim. This
/// reads the public `View`/`EntryView` assistant text directly instead of
/// unescaping `Debug`.
fn assistant_text(view: &dal_core::View) -> String {
    view.entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            dal_core::EntryKind::Assistant { content, .. } => Some(
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
        .collect()
}

/// Turns one Boolean contract check into a named failure.
fn require(condition: bool, message: String) -> support::TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// Copies one owned Starlark fixture into the scratch plugin tree.
fn install_plugin(data: &Path, plugin: &str, fixture: &str) -> support::TestResult<()> {
    let source = support::repo_root()
        .join("dalgona/gates/tests/fixtures/synthetic")
        .join(fixture);
    let directory = data.join("plugins").join(plugin);
    fs::create_dir_all(&directory)?;
    fs::copy(source, directory.join("plugin.star"))?;
    Ok(())
}

/// Writes the scripted-provider fixture and returns the user TOML selecting
/// the plugin, the synthetic default model, and the provider replay file.
fn user_config(
    data: &Path,
    script: &str,
    steps: &[String],
    plugin: &str,
    model: &str,
) -> support::TestResult<String> {
    let fixture = data.join(script);
    fs::write(&fixture, steps.join("\n") + "\n")?;
    Ok(format!(
        "approval = \"all\"\nmodel = \"{model}\"\n{NO_BATTERIES}\nplugins = \
         [\"{plugin}\"]\n\n[providers.scripted]\nfixture = {:?}\n",
        fixture.display().to_string()
    ))
}

/// Builds the composed product on `data`, pre-grants every extension's
/// injected services for the workspace (so plugin model operations never
/// stall on an interactive grant), pushes `extra`, and starts the host.
async fn start_host(
    data: &Path,
    workspace: &Path,
    user: &str,
    extra: Vec<Extension>,
) -> support::TestResult<Host> {
    let mut product = support::build_product(data.to_path_buf(), Some(user))?;
    product.extensions.extend(extra);
    for registered in &product.extensions {
        let services = registered.inject().capabilities();
        if services.is_empty() || registered.origin() == Origin::Builtin {
            continue;
        }
        let Some(key) = dal_agent::ext::grants::GrantKey::for_extension(registered, workspace)
        else {
            return Err("registered extension has no code identity".into());
        };
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(
                key,
                ClientId::new("orchestration-07"),
                dal_core::Timestamp::now(),
            )
            .await?;
    }
    let config = support::host_config(data, user)?;
    let env = Env {
        vars: BTreeMap::default(),
        cwd: data.to_path_buf(),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

/// Opens one session, submits `prompt`, and waits for its turn to end.
async fn run_turn(host: &Host, workspace: &Path, prompt: &str) -> support::TestResult<TurnOutcome> {
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace.to_path_buf())?,
                name: None,
            },
            ClientId::new("orchestration-07"),
        )
        .await?;
    let mut updates = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: prompt.into(),
            }],
        })
        .await?;
    let Reply::Accepted { turn, .. } = reply else {
        return Err(format!("the prompt was not accepted: {reply:?}").into());
    };
    let mut outcome = None;
    let mut notices: Vec<String> = Vec::new();
    while outcome.is_none() {
        let delivery = tokio::time::timeout(TURN_WAIT, updates.next()).await?;
        let Some(delivery) = delivery else {
            return Err("the update stream ended before the turn did".into());
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        if let UpdateKind::Notice(notice) = &update.kind {
            notices.push(format!("{}: {}", notice.kind, notice.text));
            continue;
        }
        if let UpdateKind::TurnEnded {
            turn: ended, stop, ..
        } = update.kind
            && ended == turn
        {
            let view = agent.view(PageReq::default())?;
            outcome = Some(TurnOutcome {
                stop,
                text: assistant_text(&view),
                entries: view
                    .entries
                    .items
                    .iter()
                    .map(|entry| entry.kind.clone())
                    .collect(),
                view: format!("{view:?}"),
                notices: notices.join("\n"),
                session: view.session.id.to_string(),
            });
        }
    }
    drop(updates);
    drop(agent);
    outcome.ok_or("the turn never reported its end".into())
}

/// Shuts the host down and requires a fully drained task tracker.
async fn shutdown(host: Host) -> support::TestResult<()> {
    let report = host.shutdown(Duration::from_secs(10)).await;
    require(
        report.tasks_remaining == 0,
        format!("shutdown left {} tasks behind", report.tasks_remaining),
    )
}

/// Collects every journaled byte under `root`, in stable order, so a test
/// can prove the journal did not change.
fn journal_snapshot(root: &Path) -> support::TestResult<Vec<(PathBuf, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut stack = vec![root.join("sessions")];
    while let Some(dir) = stack.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path)?;
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The model caps every fixture model declares.
fn caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

/// The API route the scripted provider answers.
fn api_route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-6-luna".into(),
        provider: None,
    }
}

/// One relay-shaped inference request.
fn relay_request(route: ModelRoute) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: route,
        system: "".into(),
        tools: Arc::from(Vec::new()),
        context: Arc::from(vec![ContextItem::User {
            parts: vec![Part::Text { text: "hi".into() }],
        }]),
        params: RequestParams::default(),
        cache_key: None,
    }
}

/// A bounded drain of one relay stream.
async fn relay_items(
    host: &Host,
    route: ModelRoute,
    request: ModelRequest,
) -> support::TestResult<Vec<Result<ProviderEvent, ProviderError>>> {
    let mut stream = host.relay(ClientId::new("router"), route, request).await?;
    let mut items = Vec::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(10), stream.next()).await? {
        items.push(item);
    }
    Ok(items)
}

/// The first usage event's input count, for scripted-answer assertions.
fn used_input(items: &[Result<ProviderEvent, ProviderError>]) -> Option<u64> {
    items.iter().find_map(|item| match item {
        Ok(ProviderEvent::Usage { usage }) => Some(usage.input_tokens),
        _ => None,
    })
}

/// A handler-authored stream with one text delta and a clean stop.
fn done_stream(text: &str) -> EventStream {
    let usage = dal_core::Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    };
    let items: Vec<Result<ProviderEvent, ProviderError>> = vec![
        Ok(ProviderEvent::TextDelta {
            text: text.to_string(),
        }),
        Ok(ProviderEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(ProviderEvent::Usage { usage }),
        Ok(ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
}

/// Test-local observations a Rust handler reports back to the gate.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, line: impl Into<String>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.into());
    }

    fn text(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .join("\n")
    }
}

/// A model-visible session tool the gate can see the forwarded call reach.
struct NoteTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl NoteTool {
    fn new() -> support::TestResult<Self> {
        let name = Name::parse("probe_note").map_err(|error| format!("tool name: {error}"))?;
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Records the note it is handed.".into(),
            parameters: RawJson::parse(
                r#"{"type":"object","properties":{"note":{"type":"string"}}}"#,
            )
            .map_err(|error| format!("tool schema: {error}"))?,
            grammar: None,
        });
        Ok(Self { name, spec })
    }
}

impl Tool for NoteTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Ok(Box::new(ToolOutput::from_text("note-ok"))) })
    }
}

/// Pushes the session tool the forwarded call must reach, unchanged.
fn note_tool_extension() -> support::TestResult<Extension> {
    let extension = ExtensionBuilder::new("probenote", "0.1.0", ServiceSet::EMPTY)?
        .with_origin(Origin::Bundled, None)
        .tool(Arc::new(NoteTool::new()?), Visibility::Model)
        .build()?;
    Ok(extension)
}

/// What the relay probe exercises.
#[derive(Clone, Copy)]
enum RelayProbeMode {
    /// Opens a scope and infers the inner API model through it.
    ScopeInfer,
    /// Launches a member session through the scope, which the relay denies.
    MemberSession,
}

/// A Rust-registered synthetic model driving the relay-only service surface.
struct RelayProbe {
    mode: RelayProbeMode,
    log: Log,
}

impl ModelHandler for RelayProbe {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let spec = ScopeSpec {
                limit: 2,
                on_error: OnError::Settle,
                budget: Budget::default(),
            };
            let scope = match cx.scope(spec) {
                Ok(scope) => scope,
                Err(error) => {
                    self.log.push(format!("scope-open {error:?}"));
                    return Ok(done_stream("relay-scope-refused"));
                }
            };
            match self.mode {
                RelayProbeMode::ScopeInfer => {
                    request.model = api_route();
                    let handle = match scope.infer(request) {
                        Ok(handle) => handle,
                        Err(error) => {
                            self.log.push(format!("scope-infer refused {error:?}"));
                            return Ok(done_stream("relay-infer-refused"));
                        }
                    };
                    let _ = scope.next().await;
                    let usage = handle.usage();
                    let failure = handle.result().await.err();
                    self.log.push(format!(
                        "scope-infer input {} cost {:?} failure {failure:?}",
                        usage.input_tokens, usage.cost_usd
                    ));
                    Ok(done_stream("relay-ok"))
                }
                RelayProbeMode::MemberSession => {
                    let start = AgentStart {
                        call: CallId::new("member-call"),
                        name: Some("member".into()),
                        prompt: "work".into(),
                        model: None,
                        role: None,
                        system: None,
                        tools: None,
                        workspace: None,
                    };
                    match scope.agent(start) {
                        Err(error) => match &error {
                            dal_agent::ext::ScopeError::Denied(
                                dal_core::DenyReason::Unavailable { what },
                            ) if what.as_ref() == "member sessions outside a session" => {
                                self.log.push(format!("member {error:?}"));
                            }
                            _ => {
                                self.log.push(format!("member unexpected {error:?}"));
                            }
                        },
                        Ok(_) => {
                            self.log.push("member unexpected success".to_string());
                        }
                    }
                    Ok(done_stream("relay-member"))
                }
            }
        })
    }
}

/// Pushes the two Rust relay probes sharing one observation log.
fn relay_probe_extension(log: Log) -> support::TestResult<Extension> {
    let relay_ok = ModelId::parse("dalgona/relayok").map_err(|error| error.to_string())?;
    let relay_member = ModelId::parse("dalgona/relaymember").map_err(|error| error.to_string())?;
    let extension = ExtensionBuilder::new("relayprobe", "0.1.0", ServiceSet::EMPTY)?
        .with_origin(Origin::Bundled, None)
        .model(ModelRecord {
            id: relay_ok,
            caps: caps(),
            handler: Arc::new(RelayProbe {
                mode: RelayProbeMode::ScopeInfer,
                log: log.clone(),
            }),
            export: None,
        })
        .model(ModelRecord {
            id: relay_member,
            caps: caps(),
            handler: Arc::new(RelayProbe {
                mode: RelayProbeMode::MemberSession,
                log,
            }),
            export: None,
        })
        .build()?;
    Ok(extension)
}

/// A scratch data root, its workspace, and the started host.
struct Rig {
    scratch: support::Scratch,
    workspace: PathBuf,
    host: Host,
}

/// One scripted-model scenario: the plugin fixture to install, the
/// provider replay steps, and the session's default model route.
struct Scenario {
    label: &'static str,
    plugin: &'static str,
    fixture: &'static str,
    script: &'static str,
    model: &'static str,
    steps: Vec<String>,
}

/// Installs the scenario's plugin, writes its provider steps, and starts
/// the product host on a fresh scratch tree.
async fn rig(scenario: Scenario, extra: Vec<Extension>) -> support::TestResult<Rig> {
    let scratch = support::Scratch::new(scenario.label)?;
    let data = scratch.path().to_path_buf();
    let workspace = data.join("workspace");
    fs::create_dir_all(&workspace)?;
    install_plugin(&data, scenario.plugin, scenario.fixture)?;
    let user = user_config(
        &data,
        scenario.script,
        &scenario.steps,
        scenario.plugin,
        scenario.model,
    )?;
    let host = start_host(&data, &workspace, &user, extra).await?;
    Ok(Rig {
        scratch,
        workspace,
        host,
    })
}

/// The provider steps for the contract scenario, in the order the handler
/// consumes them: the probe task, two panels, the private round, the
/// session round, and the second run's closing forward.
fn contract_steps() -> Vec<String> {
    vec![
        support::text_step("panel-ok"),
        support::text_step("panel-ok"),
        support::text_step("panel-ok"),
        support::tool_step("p1", "synth__probe", "{\"value\":\"probe-arg\"}"),
        support::tool_step("s1", "probe_note", "{\"note\":\"carry-me\"}"),
        support::text_step("contract closed"),
    ]
}

#[test]
fn synthetic_model_streaming_and_private_tools_follow_the_handler_contract()
-> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        let contract = contract_leg().await;
        let marked = marked_forward_leg().await;
        let second = second_forward_leg().await;
        let contract = contract.err().map(|error| error.to_string());
        let marked = marked.err().map(|error| error.to_string());
        let second = second.err().map(|error| error.to_string());
        require(
            contract.is_none() && marked.is_none() && second.is_none(),
            format!(
                "handler contract legs failed: contract={contract:?}; \
                 marked-forward={marked:?}; second-forward={second:?}"
            ),
        )
    });
    // A red leg can leave a blocked Starlark worker behind; bound the
    // runtime teardown instead of hanging the gate on it.
    runtime.shutdown_timeout(Duration::from_secs(15));
    outcome
}

/// The streamed evidence token that starts the handler's operation report.
const EVIDENCE: &str = "task-id=";

/// The full handler contract on the real product session loop: inner
/// results, the probed handle surface, one forward stream, a private call
/// contained in the handler, and a forwarded session tool call reaching the
/// loop unchanged.
async fn contract_leg() -> support::TestResult<()> {
    let rig = rig(
        Scenario {
            label: "orchestration-synthetic-contract",
            plugin: "synth",
            fixture: "handler-contract.star",
            script: "contract-script.jsonl",
            model: "dalgona/contract",
            steps: contract_steps(),
        },
        vec![note_tool_extension()?],
    )
    .await?;
    let outcome = run_turn(&rig.host, &rig.workspace, "run the handler contract").await?;

    require(
        outcome.stop == Stop::EndTurn,
        format!(
            "the contract turn failed: stop={:?} notices={} text={} view={}",
            outcome.stop, outcome.notices, outcome.text, outcome.view
        ),
    )?;
    // The streamed order: the handler's evidence first, then the forwarded
    // closing text.
    let evidence_at = outcome
        .view
        .find(EVIDENCE)
        .ok_or_else(|| format!("the handler surface evidence is missing: {}", outcome.view))?;
    let closed_at = outcome
        .view
        .find("contract closed")
        .ok_or_else(|| format!("the forwarded closing text is missing: {}", outcome.view))?;
    require(
        evidence_at < closed_at,
        "the forward stream must carry the handler evidence before the inner \
         closing text"
            .to_string(),
    )?;
    // The forwarded session tool call reached the loop unchanged: the same
    // name, the same argument bytes, and the tool's own result.
    require(
        outcome.view.contains("probe_note")
            && outcome.view.contains("carry-me")
            && outcome.view.contains("note-ok"),
        format!(
            "the forwarded session tool call must reach the loop unchanged: {}",
            outcome.view
        ),
    )?;
    private_call_stays_journaled(&rig, &outcome.session)?;
    handle_operations_are_consistent(&rig, &outcome)?;
    shutdown(rig.host).await
}

/// The private call stayed inside the handler: the session journal names
/// the session tool and never the private tool.
fn private_call_stays_journaled(rig: &Rig, session: &str) -> support::TestResult<()> {
    let journal = support::session_dir(rig.scratch.path(), session)
        .ok_or("the contract session journal is missing")?
        .join("journal.jsonl");
    let journal = fs::read_to_string(journal)?;
    require(
        journal.contains("probe_note") && journal.contains("carry-me"),
        "the session tool call must be journaled".to_string(),
    )?;
    require(
        !journal.contains("synth__probe"),
        format!("the private tool call leaked into the session journal: {journal}"),
    )
}

/// The handler report's keys, in the order `handler-contract.star` writes them.
const HANDLE_EVIDENCE_KEYS: [&str; 11] = [
    "task-id",
    "status-before",
    "status-after",
    "usage-input",
    "usage-output",
    "cancelled-status",
    "cancelled-error",
    "member-id",
    "member-receipt",
    "member-status",
    "member-report",
];

/// Splits the handler's operation report into its values, in key order.
///
/// Two values hold spaces (the cancellation message and the member report),
/// so each value runs to the next expected key rather than to the next space.
/// A missing, reordered, or extra key fails here instead of passing a
/// presence probe.
fn handle_evidence(report: &str) -> support::TestResult<[String; 11]> {
    let mut values = Vec::with_capacity(HANDLE_EVIDENCE_KEYS.len());
    let mut rest = report;
    for (index, key) in HANDLE_EVIDENCE_KEYS.iter().enumerate() {
        let label = format!("{key}=");
        rest = rest
            .strip_prefix(label.as_str())
            .ok_or_else(|| format!("the report must carry `{label}` next: {rest}"))?;
        let (value, tail) = match HANDLE_EVIDENCE_KEYS.get(index + 1) {
            Some(next) => {
                let separator = format!(" {next}=");
                let end = rest.find(separator.as_str()).ok_or_else(|| {
                    format!("the report must carry `{next}=` after `{key}`: {rest}")
                })?;
                (&rest[..end], &rest[end + 1..])
            }
            None => (rest, ""),
        };
        values.push(value.to_owned());
        rest = tail;
    }
    let values: [String; 11] = values
        .try_into()
        .map_err(|values: Vec<String>| format!("the report parsed {} fields", values.len()))?;
    Ok(values)
}

/// The text of every assistant entry, one string per entry.
fn assistant_entry_texts(entries: &[dal_core::EntryKind]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            dal_core::EntryKind::Assistant { content, .. } => Some(
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
        .collect()
}

/// One string member of a parsed journal record.
fn json_str<'a>(value: &'a sonic_rs::Value, key: &str) -> Option<&'a str> {
    use sonic_rs::JsonValueTrait as _;
    value.get(key).and_then(|member| member.as_str())
}

/// The joined text blocks and the tool-call block count of one journaled
/// assistant record.
fn journaled_assistant(line: &str) -> support::TestResult<(String, usize)> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let record: sonic_rs::Value = sonic_rs::from_str(line)?;
    let blocks = record
        .get("content")
        .and_then(|content| content.as_array())
        .ok_or_else(|| format!("the assistant record has no content blocks: {line}"))?;
    let mut text = String::new();
    let mut calls = 0;
    for block in blocks {
        match block.get("type").and_then(|kind| kind.as_str()) {
            Some("text") => text.push_str(
                block
                    .get("text")
                    .and_then(|part| part.as_str())
                    .unwrap_or_default(),
            ),
            Some("tool_call") => calls += 1,
            _ => {}
        }
    }
    Ok((text, calls))
}

/// The handler reported a child session that exists on disk: its own journal
/// holds the one aside the handler sent, no tool ever ran in it, and its last
/// assistant text is exactly the report the wait returned.
fn member_session_agrees(
    rig: &Rig,
    parent: &str,
    member: &str,
    receipt: &str,
    report: &str,
) -> support::TestResult<()> {
    require(
        !member.is_empty() && member != parent,
        format!("the member handle must name a child session, not the parent {parent}: {member:?}"),
    )?;
    let directory = support::session_dir(rig.scratch.path(), member)
        .ok_or_else(|| format!("the reported member session {member} has no journal"))?;
    let journal = fs::read_to_string(directory.join("journal.jsonl"))?;

    // The delivered receipt is backed by one aside in the recipient's journal.
    let mail_lines = support::journal_records(&journal, "mail");
    let [line] = mail_lines.as_slice() else {
        return Err(format!(
            "the member journal must hold exactly one mail record, found {}: {journal}",
            mail_lines.len()
        )
        .into());
    };
    let mail: sonic_rs::Value = sonic_rs::from_str(line)?;
    require(
        receipt == "delivered"
            && json_str(&mail, "to") == Some(member)
            && json_str(&mail, "mode") == Some("aside")
            && json_str(&mail, "text") == Some("carry-me"),
        format!(
            "the delivered receipt must match one aside to the member: receipt={receipt} {line}"
        ),
    )?;

    // No tool ran in the child, and its final assistant text is the report.
    require(
        support::journal_records(&journal, "tool_result").is_empty(),
        format!("a tool ran in the member session: {journal}"),
    )?;
    let assistants = support::journal_records(&journal, "assistant");
    let Some(last) = assistants.last() else {
        return Err(format!("the member journal holds no assistant record: {journal}").into());
    };
    for line in &assistants {
        let (_, calls) = journaled_assistant(line)?;
        require(calls == 0, format!("the member requested a tool: {line}"))?;
    }
    let (text, _) = journaled_assistant(last)?;
    require(
        text == report && report.starts_with("member-ready "),
        format!(
            "the member report must be the child's own final assistant text: \
             journal={text:?} report={report:?}"
        ),
    )
}

/// Exactly the one session tool the scripted provider requested ran in the
/// parent session, and it succeeded: neither the private probe nor any
/// operation the handler consumed surfaced as a session tool call.
fn only_the_session_tool_ran(entries: &[dal_core::EntryKind]) -> support::TestResult<()> {
    let mut calls: Vec<&str> = Vec::new();
    let mut results: Vec<&str> = Vec::new();
    let mut failed = 0;
    for entry in entries {
        match entry {
            dal_core::EntryKind::Assistant { content, .. } => {
                for block in content {
                    if let dal_core::Block::ToolCall { name, .. } = block {
                        calls.push(name);
                    }
                }
            }
            dal_core::EntryKind::ToolResult { name, error, .. } => {
                results.push(name);
                if *error {
                    failed += 1;
                }
            }
            _ => {}
        }
    }
    require(
        calls == ["probe_note"] && results == ["probe_note"] && failed == 0,
        format!("tool calls={calls:?} results={results:?} failed={failed}"),
    )
}

/// The handler consumed the real task and member operations. The streamed
/// report is parsed field by field and each value is held to a source the
/// handler does not write: the host's task numbering, the scripted provider's
/// measured usage (`support::text_step`: 12 input, 5 output tokens), the
/// owner-cancellation outcome, and the child session's own journal on disk.
fn handle_operations_are_consistent(rig: &Rig, outcome: &TurnOutcome) -> support::TestResult<()> {
    let entry_texts = assistant_entry_texts(&outcome.entries);
    let carriers: Vec<&String> = entry_texts
        .iter()
        .filter(|entry_text| entry_text.contains(EVIDENCE))
        .collect();
    let [report] = carriers.as_slice() else {
        return Err(format!(
            "exactly one assistant entry must carry the handler report, found {}: {}",
            carriers.len(),
            outcome.view
        )
        .into());
    };
    require(
        report.starts_with(EVIDENCE),
        format!("the handler report must open its streamed text: {report}"),
    )?;
    let [
        task_id,
        status_before,
        status_after,
        usage_input,
        usage_output,
        cancelled_status,
        cancelled_error,
        member,
        receipt,
        member_status,
        member_report,
    ] = handle_evidence(report)?;

    // The task is the first submission of the handler's own invocation, so
    // the host minted id 1 for it.
    require(
        task_id == "1",
        format!("the task handle must carry the host-minted id 1: {task_id:?}"),
    )?;
    require(
        matches!(status_before.as_str(), "pending" | "running" | "done"),
        format!("the live task reported an impossible status: {status_before:?}"),
    )?;
    require(
        status_after == "done",
        format!("the settled task must report done: {status_after:?}"),
    )?;
    require(
        usage_input == "12" && usage_output == "5",
        format!(
            "the task usage must be the provider's 12 input and 5 output: {usage_input:?} {usage_output:?}"
        ),
    )?;
    require(
        cancelled_status == "cancelled" && cancelled_error.to_ascii_lowercase().contains("cancel"),
        format!(
            "the cancelled task must expose the owner cancellation: {cancelled_status:?} {cancelled_error:?}"
        ),
    )?;
    require(
        member_status == "done",
        format!("the settled member start must report done: {member_status:?}"),
    )?;
    member_session_agrees(rig, &outcome.session, &member, &receipt, &member_report)?;
    only_the_session_tool_ran(&outcome.entries)
}

/// A marked inner scope inference uses the real forward path and therefore
/// retains private-tool containment.
async fn marked_forward_leg() -> support::TestResult<()> {
    let rig = rig(
        Scenario {
            label: "orchestration-synthetic-marked-forward",
            plugin: "synth",
            fixture: "handler-contract.star",
            script: "marked-script.jsonl",
            model: "dalgona/marked",
            steps: vec![
                support::tool_step("p1", "synth__probe", "{\"value\":\"marked\"}"),
                support::text_step("marked closed"),
            ],
        },
        Vec::new(),
    )
    .await?;
    let outcome = run_turn(&rig.host, &rig.workspace, "run marked forward").await?;
    require(
        outcome.stop == Stop::EndTurn,
        format!("the marked-forward turn failed: {:?}", outcome.view),
    )?;
    require(
        outcome.view.contains("marked closed"),
        format!(
            "the marked forward result did not reach the consumer: {}",
            outcome.view
        ),
    )?;
    let journal = support::session_dir(rig.scratch.path(), &outcome.session)
        .ok_or("the marked-forward session journal is missing")?
        .join("journal.jsonl");
    let journal = fs::read_to_string(journal)?;
    require(
        !journal.contains("synth__probe"),
        format!("the marked private tool leaked into the session journal: {journal}"),
    )?;
    shutdown(rig.host).await
}

/// A second forward in one handler run is the named typed error, surfaced
/// through the real product turn.
async fn second_forward_leg() -> support::TestResult<()> {
    let rig = rig(
        Scenario {
            label: "orchestration-synthetic-second-forward",
            plugin: "synth",
            fixture: "handler-contract.star",
            script: "double-script.jsonl",
            model: "dalgona/double",
            steps: vec![support::text_step("forwarded")],
        },
        Vec::new(),
    )
    .await?;
    let outcome = run_turn(&rig.host, &rig.workspace, "force a second forward").await?;
    require(
        outcome.stop == Stop::EndTurn,
        format!("the second-forward turn failed: {:?}", outcome.view),
    )?;
    require(
        outcome
            .view
            .contains("second-forward-refused: forward may be called at most once"),
        format!(
            "the second forward must yield the named error: {}",
            outcome.view
        ),
    )?;
    shutdown(rig.host).await
}

#[test]
fn synthetic_model_cycles_and_depth_are_typed_errors() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        let cycle = cycle_leg().await;
        let depth = depth_leg().await;
        let cycle = cycle.err().map(|error| error.to_string());
        let depth = depth.err().map(|error| error.to_string());
        require(
            cycle.is_none() && depth.is_none(),
            format!("cycle/depth legs failed: cycle={cycle:?}; depth={depth:?}"),
        )
    });
    runtime.shutdown_timeout(Duration::from_secs(15));
    outcome
}

/// A scope child that re-infers its own id is the typed cycle with the
/// exact two-route chain.
async fn cycle_leg() -> support::TestResult<()> {
    let rig = rig(
        Scenario {
            label: "orchestration-synthetic-cycle",
            plugin: "synthcyc",
            fixture: "cycle-depth.star",
            script: "cycle-script.jsonl",
            model: "dalgona/cycle",
            steps: vec![support::text_step("unused")],
        },
        Vec::new(),
    )
    .await?;
    let outcome = run_turn(&rig.host, &rig.workspace, "infer this model from itself").await?;
    require(
        outcome.stop == Stop::EndTurn,
        format!("the cycle turn failed: {:?}", outcome.view),
    )?;
    require(
        outcome.text.contains(
            "synthetic model cycle: [Synthetic { id: \"dalgona/cycle\" }, \
             Synthetic { id: \"dalgona/cycle\" }]",
        ),
        format!("the typed cycle chain drifted: {}", outcome.text),
    )?;
    shutdown(rig.host).await
}

/// A scope chain reaching a fifth id is the typed depth error with the
/// exact five-route chain, and the fifth handler never runs.
async fn depth_leg() -> support::TestResult<()> {
    let rig = rig(
        Scenario {
            label: "orchestration-synthetic-depth",
            plugin: "synthcyc",
            fixture: "cycle-depth.star",
            script: "depth-script.jsonl",
            model: "dalgona/dd1",
            steps: vec![support::text_step("unused")],
        },
        Vec::new(),
    )
    .await?;
    let outcome = run_turn(&rig.host, &rig.workspace, "nest five synthetic ids").await?;
    require(
        outcome.stop == Stop::EndTurn,
        format!("the depth turn failed: {:?}", outcome.view),
    )?;
    require(
        outcome.text.contains("ran dalgona/dd4"),
        format!("the fourth handler must run: {}", outcome.text),
    )?;
    require(
        !outcome.text.contains("ran dalgona/dd5"),
        format!(
            "the fifth handler is refused before it runs: {}",
            outcome.text
        ),
    )?;
    require(
        outcome.text.contains(
            "synthetic model depth exceeds 4: [Synthetic { id: \"dalgona/dd1\" }, \
             Synthetic { id: \"dalgona/dd2\" }, Synthetic { id: \"dalgona/dd3\" }, \
             Synthetic { id: \"dalgona/dd4\" }, Synthetic { id: \"dalgona/dd5\" }]",
        ),
        format!("the typed depth chain drifted: {}", outcome.text),
    )?;
    shutdown(rig.host).await
}

/// The relay route names, probed in order.
const RELAY_OK: &str = "dalgona/relayok";
const RELAY_MEMBER: &str = "dalgona/relaymember";
const RELAY_STAR: &str = "dalgona/relaystar";

/// The member-session denial the relay services return.
const MEMBER_DENIAL: &str =
    "member Denied(Unavailable { what: \"member sessions outside a session\" })";

/// The relay rig's scripted step, in the public replay grammar: one
/// response measuring 12 input tokens, 5 output tokens, and the
/// provider-reported $0.001 the fixture authors. The scope child and the
/// Starlark relay forward each consume one copy.
const RELAY_STEP: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"relay-ok"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":12,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":0.001}},{"type":"stop","reason":"end_turn"}]}"#;

/// Installs the relay fixture and both Rust relay probes on one host with
/// no session, recording the journaled bytes before any relay call.
async fn relay_rig() -> support::TestResult<(Rig, Log, Vec<(PathBuf, Vec<u8>)>)> {
    let log = Log::default();
    let rig = rig(
        Scenario {
            label: "orchestration-relay-denials",
            plugin: "synthrelay",
            fixture: "relay-probe.star",
            script: "relay-script.jsonl",
            model: "dalgona/relayok",
            steps: vec![RELAY_STEP.to_owned(), RELAY_STEP.to_owned()],
        },
        vec![relay_probe_extension(log.clone())?],
    )
    .await?;
    let before = journal_snapshot(rig.scratch.path())?;
    require(
        before
            .iter()
            .all(|(path, _)| path.ends_with("journal.jsonl")),
        format!("unexpected files under the session store: {before:?}"),
    )?;
    Ok((rig, log, before))
}

#[test]
fn relay_model_scope_denies_member_sessions_without_journaling() -> support::TestResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = runtime.block_on(async {
        let (rig, log, before) = relay_rig().await?;

        // The relay permits scopes and inference: the scope child runs and
        // reports the scripted usage.
        let items = relay_items(
            &rig.host,
            ModelRoute::from_id(RELAY_OK),
            relay_request(ModelRoute::from_id(RELAY_OK)),
        )
        .await?;
        require(
            matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))),
            format!("the relay scope inference must end cleanly: {items:?}"),
        )?;
        require(
            used_input(&items) == Some(1),
            format!("the relayed probe response must report its usage: {items:?}"),
        )?;
        let text = log.text();
        require(
            text.contains("scope-infer input 12 cost Some(0.001) failure None"),
            format!("the relayed inner inference must report the scripted priced usage: {text}"),
        )?;

        // Member sessions are denied with the relay's exact unavailable
        // reason.
        let items = relay_items(
            &rig.host,
            ModelRoute::from_id(RELAY_MEMBER),
            relay_request(ModelRoute::from_id(RELAY_MEMBER)),
        )
        .await?;
        require(
            matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))),
            format!("the denial probe must end cleanly: {items:?}"),
        )?;
        let text = log.text();
        require(
            text.contains(MEMBER_DENIAL),
            format!("the member-session denial drifted: {text}"),
        )?;
        // The fixture model itself must run under the relay: its forward
        // carries the scripted usage through the relay stream. The error is
        // captured so the member and journal legs still prove their green,
        // and only this leg names the typed relay gap.
        let starlark = starlark_relay_reports_the_scripted_usage(&rig)
            .await
            .err()
            .map(|error| error.to_string());

        // The journal is byte-identical: the relay appended nothing and
        // created no session.
        let after = journal_snapshot(rig.scratch.path())?;
        require(
            before == after,
            format!(
                "the relay must not touch the journal: before={before:?} \
                 after={after:?}"
            ),
        )?;
        require(
            starlark.is_none(),
            format!("the Starlark relay leg failed: {starlark:?}"),
        )?;
        shutdown(rig.host).await
    });
    runtime.shutdown_timeout(Duration::from_secs(15));
    outcome
}

/// The actual Starlark model runs under `Host::relay` and its forward
/// carries the scripted usage through the relay stream. Until the relay
/// attaches the script context this fails typed at the real requirement,
/// naming the remaining dal-star gap; it must not be repinned to match
/// the current debug output.
async fn starlark_relay_reports_the_scripted_usage(rig: &Rig) -> support::TestResult<()> {
    let items = relay_items(
        &rig.host,
        ModelRoute::from_id(RELAY_STAR),
        relay_request(ModelRoute::from_id(RELAY_STAR)),
    )
    .await?;
    require(
        matches!(items.last(), Some(Ok(ProviderEvent::Stop { .. }))),
        format!(
            "the relayed Starlark model must run under Host::relay; the \
             relay attaches no script context today, so dal-star model.rs \
             refuses with `script context is unavailable for this model` \
             (private source A/B pending), and its stream stays a failure: \
             {items:?}"
        ),
    )?;
    require(
        used_input(&items) == Some(12),
        format!("the relayed Starlark forward must report the scripted usage: {items:?}"),
    )
}
