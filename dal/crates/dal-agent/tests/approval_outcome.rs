//! Approval outcomes at the dispatch boundary: a client's decline and a
//! deadline with nobody answering reach the model as different results, and
//! both fail closed without running the tool.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part, Preview,
    RawJson, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};

/// Bounds each real-clock wait in the approval tests.
const WAIT: Duration = Duration::from_secs(3600);

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";

const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A model-visible tool that asks for approval and, once approved, reports it.
struct ApprovalProbe {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for ApprovalProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Exec {
            read_only: false,
            grant: None,
        })
    }

    fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let preview = Preview {
                title: "approval-probe".into(),
                body: "printf approved".into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(ToolOutput::from_text("the probe ran")),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

struct Session {
    agent: Agent,
    subscription: Subscription,
    _tmp: tempfile::TempDir,
}

/// Starts one host with an answering client attached and one scripted turn
/// that calls the probe tool under the default ask policy.
async fn start_turn() -> Result<Session, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, format!("{STEP_CALL}{STEP_END}"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
    let probe = Arc::new(ApprovalProbe {
        name: Name::parse("fixture__probe")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__probe")?,
            description: "approval probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
    });
    let extension = ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .script_tool(
            probe,
            Visibility::Model,
            ExportSpec {
                id: ExportId {
                    plugin: Name::parse("fixture")?,
                    kind: ExportKind::Tool,
                    local: Name::parse("probe")?,
                },
                uses: OpSet::EMPTY,
                input: RawJson::parse(r#"{"type":"object"}"#)?,
                description: "approval probe export".into(),
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
    let subscription = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the probe".into(),
            }],
        })
        .await?;
    assert!(
        matches!(reply, dal_core::Reply::Accepted { .. }),
        "prompt not accepted: {reply:?}"
    );
    Ok(Session {
        agent,
        subscription,
        _tmp: tmp,
    })
}

async fn next_delivery(
    subscription: &mut Subscription,
    wait: Duration,
) -> Result<Delivery, Box<dyn std::error::Error>> {
    tokio::time::timeout(wait, subscription.next())
        .await?
        .ok_or_else(|| "the session closed before the expected update".into())
}

/// Reads updates until the approval request opens and returns its id.
async fn next_request(
    agent: &Agent,
    subscription: &mut Subscription,
    wait: Duration,
) -> Result<dal_core::RequestId, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = next_delivery(subscription, wait).await?;
        if let Delivery::Update(update) = &delivery
            && let UpdateKind::RequestOpened(request) = &update.kind
        {
            for _ in 0..200 {
                if agent
                    .view(dal_core::PageReq::default())?
                    .open
                    .iter()
                    .any(|open| open.id == request.id)
                {
                    return Ok(request.id);
                }
                tokio::task::yield_now().await;
            }
            return Err("the approval request did not enter the session view".into());
        }
    }
    Err("no approval request opened".into())
}

/// Reads updates until the turn ends and returns the final view text.
async fn finish_turn(
    session: &mut Session,
    wait: Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = next_delivery(&mut session.subscription, wait).await?;
        if let Delivery::Update(update) = &delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            let view = session.agent.view(dal_core::PageReq::default())?;
            return Ok(format!("{view:?}"));
        }
    }
    Err("the turn never ended".into())
}

#[tokio::test]
async fn a_client_decline_is_reported_as_a_decision_by_that_client() -> TestResult {
    let mut session = start_turn().await?;
    let request = next_request(&session.agent, &mut session.subscription, WAIT).await?;
    session.agent.answer(request, Answer::Decline).await?;
    let dump = finish_turn(&mut session, WAIT).await?;
    assert!(
        dump.contains("Permission denied: fixture__probe was declined by probe."),
        "{dump}"
    );
    assert!(!dump.contains("the probe ran"), "{dump}");
    assert!(!dump.contains("no one answered"), "{dump}");
    Ok(())
}

#[tokio::test]
async fn an_approval_nobody_answers_is_reported_as_unanswered_and_fails_closed() -> TestResult {
    let mut session = start_turn().await?;
    next_request(&session.agent, &mut session.subscription, WAIT).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    let dump = finish_turn(&mut session, WAIT).await?;
    assert!(
        dump.contains(
            "Permission denied: fixture__probe needed approval and no one answered within 300 s."
        ),
        "{dump}"
    );
    assert!(!dump.contains("the probe ran"), "{dump}");
    assert!(!dump.contains("was declined by"), "{dump}");
    Ok(())
}
