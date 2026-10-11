#![expect(clippy::expect_used, reason = "SC test")]

//! Service-grant checks through the host-minted extension context.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    collections::HashMap,
    error::Error,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::{
    Delivery, Env, ServiceError, SessionRef,
    ext::{
        ArgError, BoxFuture, Caller, CommandCx, CommandHandler, Extension, ExtensionBuilder,
        RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
    },
};
use dal_core::{
    AgentsOp, AgentsReply, Answer, Command, CommandName, CommandSpec, Config, ConfigProduct,
    DenyReason, Expect, JobsOp, JobsReply, ModelInfo, Name, Origin, Output, Part, RawJson, Reply,
    Service, ServiceSet, SidecarName, SidecarOp, ToolClass, ToolSpec, TurnOp, TurnOpReply,
    Visibility, Workspace,
};
use support::{GateHarness, TestDir, scripted_session};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Probe {
    Agents,
    Jobs,
    Turn,
    Sidecar,
}

const PROBES: [Probe; 4] = [Probe::Agents, Probe::Jobs, Probe::Turn, Probe::Sidecar];

type ProbeResult = Result<String, ServiceError>;

#[derive(Default)]
struct ProbeState(Mutex<Option<ProbeResult>>);

impl ProbeState {
    fn set(&self, value: ProbeResult) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value);
    }

    fn take(&self) -> Option<ProbeResult> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl Probe {
    fn label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Jobs => "jobs",
            Self::Turn => "turn",
            Self::Sidecar => "sidecar",
        }
    }

    async fn invoke(self, services: &dyn Services, caller: &Caller) -> ProbeResult {
        match self {
            Self::Agents => match services.agents(caller, AgentsOp::List).await? {
                AgentsReply::Listed(items) => Ok(format!("agents:{}", items.len())),
                _ => Err(ServiceError::failed(
                    Some(Service::Agents),
                    "unexpected agents reply",
                )),
            },
            Self::Jobs => match services.jobs(caller, JobsOp::List).await? {
                JobsReply::Listed(items) => Ok(format!("jobs:{}", items.len())),
                _ => Err(ServiceError::failed(
                    Some(Service::Jobs),
                    "unexpected jobs reply",
                )),
            },
            Self::Turn => match services.turn(caller, TurnOp::IsIdle).await? {
                TurnOpReply::Idle(idle) => Ok(format!("turn-idle:{idle}")),
                _ => Err(ServiceError::failed(
                    Some(Service::Turn),
                    "unexpected turn reply",
                )),
            },
            Self::Sidecar => {
                let name = SidecarName::parse("gate-slot").expect("static sidecar name");
                services
                    .sidecar(
                        caller,
                        SidecarOp::Write {
                            name: name.clone(),
                            bytes: b"approved".to_vec(),
                        },
                    )
                    .await?;
                match services.sidecar(caller, SidecarOp::Read { name }).await? {
                    Some(bytes) if bytes == b"approved" => Ok("sidecar:approved".into()),
                    _ => Err(ServiceError::failed(
                        Some(Service::Sidecar),
                        "sidecar value did not persist",
                    )),
                }
            }
        }
    }
}

struct ProbeCommand {
    probe: Probe,
    state: Arc<ProbeState>,
}

impl CommandHandler for ProbeCommand {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        let probe = self.probe;
        let caller = cx.caller().clone();
        let services = Arc::clone(cx.services());
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            state.set(probe.invoke(services.as_ref(), &caller).await);
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

struct ProbeTool {
    name: Name,
    spec: Arc<ToolSpec>,
    probe: Probe,
    state: Arc<ProbeState>,
}

impl Tool for ProbeTool {
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
        let probe = self.probe;
        let caller = cx.caller().clone();
        let services = cx.services();
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let result = probe.invoke(services.as_ref(), &caller).await;
            let output = match &result {
                Ok(value) => value.clone(),
                Err(error) => error.to_string(),
            };
            state.set(result);
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(output)))
        })
    }
}

type ProbeStates = HashMap<Probe, Arc<ProbeState>>;

fn build_extension(
    name: &str,
    probes: &[Probe],
    inject: ServiceSet,
) -> Result<(Extension, ProbeStates), Box<dyn Error + Send + Sync>> {
    let mut builder = ExtensionBuilder::new(name, "0.1.0", inject)?.with_origin(Origin::User, None);
    let mut states = HashMap::new();
    for probe in probes {
        let state = Arc::new(ProbeState::default());
        let command_name = format!("{name}-{}", probe.label());
        builder = builder.command(
            CommandSpec {
                name: CommandName::parse(&command_name)?,
                summary: format!("Run the {} service gate probe.", probe.label()).into(),
                args_hint: None,
            },
            Arc::new(ProbeCommand {
                probe: *probe,
                state: Arc::clone(&state),
            }),
        );
        let tool_name = Name::parse(&format!("{name}-{}-tool", probe.label()))?;
        let spec = Arc::new(ToolSpec {
            name: tool_name.clone(),
            description: format!("Probe the {} service grant.", probe.label()).into(),
            parameters: RawJson::parse(r#"{"type":"object","additionalProperties":false}"#)?,
            grammar: None,
        });
        builder = builder.tool(
            Arc::new(ProbeTool {
                name: tool_name,
                spec,
                probe: *probe,
                state: Arc::clone(&state),
            }),
            Visibility::Model,
        );
        states.insert(*probe, state);
    }
    Ok((builder.build()?, states))
}

struct RunningHarness {
    _data: TestDir,
    _workspace: TestDir,
    harness: GateHarness,
}

async fn start_session(
    extensions: Vec<Extension>,
    replay: String,
) -> Result<RunningHarness, Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = data.path().join("service-gate.jsonl");
    fs::write(&fixture, replay)?;
    let factory = dalgon::product();
    let user = format!(
        "model = \"openai-responses/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
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
    product.extensions.extend(extensions);
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::New {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
        name: None,
    };
    let harness = scripted_session(product, config, env, session).await?;
    Ok(RunningHarness {
        _data: data,
        _workspace: workspace,
        harness,
    })
}

fn scripted_tool_call(name: &str) -> String {
    let value = sonic_rs::json!({
        "kind": "events",
        "events": [
            {"type": "tool_call_started", "id": "gate-call", "name": name},
            {"type": "tool_calls_done", "calls": [{
                "id": "gate-call",
                "name": name,
                "args": {"kind": "parsed", "value": {}}
            }]},
            {"type": "usage", "usage": {
                "input_tokens": 1,
                "cached_input_tokens": 0,
                "output_tokens": 1,
                "reasoning_tokens": null,
                "cache_write_tokens": 0,
                "cost_usd": null
            }},
            {"type": "stop", "reason": "tool_use"}
        ]
    });
    sonic_rs::to_string(&value).expect("scripted tool fixture encodes as JSON")
}

fn scripted_text(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}}},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

fn replay(responses: &[String]) -> String {
    format!("{}\n", responses.join("\n"))
}

async fn run_command(
    harness: &GateHarness,
    command: &str,
    state: &ProbeState,
) -> Result<ProbeResult, Box<dyn Error + Send + Sync>> {
    let reply = tokio::time::timeout(
        Duration::from_secs(30),
        harness.agent.submit(Command::Run {
            name: command.into(),
            args: String::new().into(),
            expected: None,
        }),
    )
    .await??;
    assert!(matches!(reply, Reply::Done(_)));
    state
        .take()
        .ok_or_else(|| std::io::Error::other("service command did not record its result").into())
}

#[derive(Debug)]
struct GrantRequest {
    extension: String,
    capabilities: Vec<String>,
}

async fn run_tool_with_approval(
    harness: &GateHarness,
    probe: Probe,
) -> Result<Vec<GrantRequest>, Box<dyn Error + Send + Sync>> {
    let mut subscription = harness.agent.subscribe(None)?;
    let prompt = tokio::time::timeout(
        Duration::from_secs(30),
        harness.agent.submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: format!("Call the {} capability probe.", probe.label()).into(),
            }],
        }),
    )
    .await
    .map_err(|_| {
        std::io::Error::other(format!("{} grant probe prompt timed out", probe.label()))
    })??;
    assert!(matches!(prompt, Reply::Accepted { .. }));
    let mut requests = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), subscription.next())
            .await
            .map_err(|_| {
                std::io::Error::other(format!(
                    "{} grant probe timed out waiting for an update",
                    probe.label()
                ))
            })?
            .ok_or_else(|| std::io::Error::other("session update stream ended before turn end"))?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            dal_core::UpdateKind::RequestOpened(request) => match &request.question {
                dal_core::Question::Grant {
                    ext, capabilities, ..
                } => {
                    requests.push(GrantRequest {
                        extension: ext.to_string(),
                        capabilities: capabilities
                            .iter()
                            .map(std::string::ToString::to_string)
                            .collect(),
                    });
                    harness
                        .agent
                        .answer(request.id, Answer::ApproveForSession)
                        .await?;
                }
                _ => harness.agent.answer(request.id, Answer::Decline).await?,
            },
            dal_core::UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok(requests)
}

#[tokio::test]
async fn each_extension_service_requires_its_own_grant() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let (empty, empty_states) = build_extension("gate-no-inject", &PROBES, ServiceSet::EMPTY)?;
    let mut extensions = vec![empty];
    let mut declared_states = HashMap::new();
    let mut declared_names = HashMap::new();
    for probe in PROBES {
        let name = format!("gate-declared-{}", probe.label());
        let inject = ServiceSet::from_names([probe.label()])?;
        let (extension, states) = build_extension(&name, &[probe], inject)?;
        declared_states.insert(probe, Arc::clone(states.get(&probe).unwrap()));
        declared_names.insert(probe, name);
        extensions.push(extension);
    }
    let denied_replay = replay(&[scripted_text("no inference call is expected")]);
    let denied = start_session(extensions, denied_replay).await?;
    let mut failures = Vec::new();
    for probe in PROBES {
        let result = run_command(
            &denied.harness,
            &format!("gate-no-inject-{}", probe.label()),
            empty_states.get(&probe).unwrap(),
        )
        .await?;
        if result != Err(ServiceError::Denied(DenyReason::NotInjected)) {
            failures.push(format!("{} without injection: {result:?}", probe.label()));
        }
    }
    for probe in PROBES {
        let name = &declared_names[&probe];
        let result = run_command(
            &denied.harness,
            &format!("{name}-{}", probe.label()),
            declared_states.get(&probe).unwrap(),
        )
        .await?;
        if !matches!(
            result,
            Err(ServiceError::Denied(DenyReason::ServiceNotGranted { .. }))
        ) {
            failures.push(format!("{} without approval: {result:?}", probe.label()));
        }
    }
    let _ = denied.harness.host.shutdown(Duration::from_secs(1)).await;

    let mut allowed_extensions = Vec::new();
    let mut allowed_states = HashMap::new();
    let mut allowed_names = HashMap::new();
    for probe in PROBES {
        let name = format!("gate-approved-{}", probe.label());
        let inject = ServiceSet::from_names([probe.label()])?;
        let (extension, states) = build_extension(&name, &[probe], inject)?;
        allowed_states.insert(probe, Arc::clone(states.get(&probe).unwrap()));
        allowed_names.insert(probe, name);
        allowed_extensions.push(extension);
    }
    let mut responses = Vec::new();
    for probe in PROBES {
        let name = format!("{}-{}-tool", allowed_names[&probe], probe.label());
        responses.push(scripted_tool_call(&name));
        responses.push(scripted_text("capability probe completed"));
    }
    let allowed = start_session(allowed_extensions, replay(&responses)).await?;
    for probe in PROBES {
        let requests = run_tool_with_approval(&allowed.harness, probe).await?;
        if requests.len() != 1
            || requests[0].extension != allowed_names[&probe]
            || requests[0].capabilities != [probe.label().to_owned()]
        {
            failures.push(format!(
                "{} received an unexpected grant request: {requests:?}",
                probe.label()
            ));
        }
        let result = allowed_states[&probe]
            .take()
            .ok_or_else(|| std::io::Error::other("service tool did not record its result"))?;
        let succeeded = match (probe, result) {
            (Probe::Agents, Ok(value)) => value.starts_with("agents:"),
            (Probe::Jobs, Ok(value)) => value.starts_with("jobs:"),
            (Probe::Turn, Ok(value)) => value.starts_with("turn-idle:"),
            (Probe::Sidecar, Ok(value)) => value == "sidecar:approved",
            (_, Err(error)) => {
                failures.push(format!("{} after approval: {error:?}", probe.label()));
                false
            }
        };
        if !succeeded {
            failures.push(format!(
                "{} returned an unexpected approved result",
                probe.label()
            ));
        }
    }
    let _ = allowed.harness.host.shutdown(Duration::from_secs(1)).await;
    assert!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}
