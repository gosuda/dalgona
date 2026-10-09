//!
//!
//! The extension service ladder must ask like the exec tool: under `ask` an
//! attached front end sees one approval request for the run and the call
//! proceeds after approval; with nobody attached it fails closed with a
//! clear message; under `all` it runs without asking.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx, ToolOutcome,
    ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ClientId, Command, Expect, ModelInfo, Name, Part, RawJson, RunRequest, ToolClass,
    ToolSpec, UpdateKind, Visibility, Workspace,
};

/// Short enough that the RED run fails fast instead of hanging on a
/// request that never opens.
const WAIT: Duration = Duration::from_secs(15);

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"run-call\",\"name\":\"fixture__do_run\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"run-call\",\"name\":\"fixture__do_run\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A model-visible tool that runs `echo hello` through the extension
/// `run` service and reports whatever the service returns.
struct RunProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for RunProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(
        &self,
        _args: &RawValue,
        _ws: &Workspace,
    ) -> Result<ToolClass, dal_agent::ext::ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let request = RunRequest {
                argv: vec![OsString::from("echo"), OsString::from("hello")],
                cwd: None,
                stdin: None,
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: 4096,
            };
            match cx.services().run(cx.caller(), request).await {
                Ok(output) => ToolOutcome::Ok(ToolOutput::from_text(
                    String::from_utf8_lossy(&output.stdout_tail)
                        .into_owned()
                        .into_boxed_str(),
                )),
                Err(error) => ToolOutcome::Err(ToolError::message(error.to_string())),
            }
        })
    }
}
struct Session {
    agent: Agent,
    subscription: Option<Subscription>,
    listener: Subscription,
    _tmp: tempfile::TempDir,
}

/// Appends one line to the TOML user config under construction.
fn write_toml_line(into: &mut String, line: &str) {
    into.push_str(line);
    into.push('\n');
}

/// Builds the host, opens a session, and submits the probe prompt.
async fn host_with_probe(
    approval: Option<&str>,
) -> Result<(Agent, tempfile::TempDir), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let mut user = String::from("model = \"openai/gpt-6-luna\"\n");
    if let Some(mode) = approval {
        write_toml_line(&mut user, &format!("approval = \"{mode}\""));
    }
    write_toml_line(
        &mut user,
        &format!(
            "\n[providers.scripted]\nfixture = \"{}\"",
            fixture.to_string_lossy().replace('\\', "\\\\")
        ),
    );
    let config = dal_core::Config::load(
        dal_core::ConfigProduct::Dalgon,
        &data,
        "",
        Some(user.as_str()),
    )?;
    let probe = Arc::new(RunProbe {
        name: Name::parse("fixture__do_run")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__do_run")?,
            description: "run probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
    });
    let inject = dal_core::ServiceSet::from_names(["run"])?;
    let extension = ExtensionBuilder::new("fixture", "0.1.0", inject)?
        .with_origin(dal_core::Origin::Builtin, None)
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture")?,
                    kind: ExportKind::Tool,
                    local: Name::parse("do_run")?,
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#)?,
                description: "run probe export".into(),
            },
        )
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
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
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir)?,
                name: None,
            },
            ClientId::new("probe"),
        )
        .await?;
    Ok((agent, tmp))
}

async fn submit_probe(agent: &Agent) -> TestResult {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run echo".into(),
            }],
        })
        .await?;
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    Ok(())
}

/// Default `ask` mode with an answering client attached.
async fn start_ask_answered() -> Result<Session, Box<dyn std::error::Error>> {
    let (agent, tmp) = host_with_probe(None).await?;
    let subscription = agent.subscribe(None)?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        agent,
        subscription: Some(subscription),
        listener,
        _tmp: tmp,
    })
}

/// Default `ask` mode with only a listener: nobody can answer.
async fn start_ask_headless() -> Result<Session, Box<dyn std::error::Error>> {
    let (agent, tmp) = host_with_probe(None).await?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        agent,
        subscription: None,
        listener,
        _tmp: tmp,
    })
}

/// `all` mode with only a listener.
async fn start_all_headless() -> Result<Session, Box<dyn std::error::Error>> {
    let (agent, tmp) = host_with_probe(Some("all")).await?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        agent,
        subscription: None,
        listener,
        _tmp: tmp,
    })
}

fn run_request_id(delivery: &Delivery) -> Option<dal_core::RequestId> {
    let Delivery::Update(update) = delivery else {
        return None;
    };
    let UpdateKind::RequestOpened(request) = &update.kind else {
        return None;
    };
    let dal_core::Question::Approval { tool, .. } = &request.question else {
        return None;
    };
    (&**tool == "run").then_some(request.id)
}

/// Reads both subscriptions until the run approval request opens.
async fn next_run_request(
    session: &mut Session,
) -> Result<dal_core::RequestId, Box<dyn std::error::Error>> {
    let Some(subscription) = session.subscription.as_mut() else {
        return Err("no answerer attached".into());
    };
    tokio::time::timeout(WAIT, async {
        loop {
            for next in [subscription.next(), session.listener.next()] {
                let delivery = next
                    .await
                    .ok_or("the session closed before an approval opened")?;
                if let Some(id) = run_request_id(&delivery) {
                    return Ok(id);
                }
            }
        }
    })
    .await
    .map_err(|_| "no run approval request opened")?
}

/// Reads the listener until the turn ends and returns the final view text.
async fn finish_turn(session: &mut Session) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, session.listener.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        let Delivery::Update(update) = &delivery else {
            continue;
        };
        if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
            let view = session.agent.view(dal_core::PageReq::default())?;
            return Ok(format!("{view:?}"));
        }
    }
    Err("the turn never ended".into())
}

#[tokio::test]
async fn a_granted_run_under_ask_asks_and_runs_after_approval() -> TestResult {
    let mut session = start_ask_answered().await?;
    let request = next_run_request(&mut session).await?;
    session.agent.answer(request, Answer::Approve).await?;
    let dump = finish_turn(&mut session).await?;
    assert!(dump.contains("hello"), "the approved run ran: {dump}");
    assert!(!dump.contains("denied"), "{dump}");
    Ok(())
}

#[tokio::test]
async fn a_granted_run_under_ask_fails_closed_with_a_clear_message() -> TestResult {
    let mut session = start_ask_headless().await?;
    let dump = finish_turn(&mut session).await?;
    assert!(
        !dump.contains("hello"),
        "nothing ran without approval: {dump}"
    );
    assert!(
        dump.contains("Permission denied: run needs approval"),
        "the denial names the fix: {dump}"
    );
    Ok(())
}

#[tokio::test]
async fn a_granted_run_under_all_runs_without_asking() -> TestResult {
    let mut session = start_all_headless().await?;
    let dump = finish_turn(&mut session).await?;
    assert!(dump.contains("hello"), "the run needed no approval: {dump}");
    assert!(!dump.contains("denied"), "{dump}");
    Ok(())
}
