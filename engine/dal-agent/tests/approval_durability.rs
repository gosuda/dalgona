//! Approval durability at the dispatch boundary: the actor folds an
//! approval before any client can answer it, session-level approvals replay
//! after resume, and a failed journal append runs no provider effect.

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
    Answer, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, ModelInfo, Name, Part,
    RawJson, Record, SessionId, Stop, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};
use dal_store::Store;

const WAIT: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

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
            let preview = dal_core::Preview {
                title: "approval-probe".into(),
                body: "printf approved".into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("the probe ran"))),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

struct Session {
    host: Host,
    agent: Agent,
    subscription: Subscription,
    tmp: tempfile::TempDir,
}

async fn start_session(turns: usize) -> Result<Session, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
    let mut script = String::new();
    for _ in 0..turns {
        script.push_str(STEP_CALL);
        script.push_str(STEP_END);
    }
    std::fs::write(&fixture, script)?;
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
    Ok(Session {
        host,
        agent,
        subscription,
        tmp,
    })
}

async fn submit_prompt(agent: &Agent) -> TestResult {
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
    Ok(())
}

async fn next_opened(
    subscription: &mut Subscription,
) -> Result<dal_core::RequestId, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before an approval opened")?;
        if let Delivery::Update(update) = &delivery
            && let UpdateKind::RequestOpened(request) = &update.kind
        {
            return Ok(request.id);
        }
    }
    Err("no approval request opened".into())
}

async fn finish_turn(
    session: &mut Session,
) -> Result<(String, Option<Stop>), Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, session.subscription.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        if let Delivery::Update(update) = &delivery
            && let UpdateKind::TurnEnded { stop, .. } = update.kind
        {
            let view = session.agent.view(dal_core::PageReq::default())?;
            return Ok((format!("{view:?}"), Some(stop)));
        }
    }
    Err("the turn never ended".into())
}

async fn journal_records(
    tmp: &tempfile::TempDir,
    session: SessionId,
) -> Result<Vec<Record>, Box<dyn std::error::Error>> {
    let store = Store::new(
        tmp.path().join("data"),
        Workspace::new(tmp.path().join("w"))?,
        dal_core::Product::Dal,
    );
    let (journal, _) = store.open_session(session).await?;
    Ok(journal.records().to_vec())
}

fn journaled_answer(records: &[Record], request: dal_core::RequestId) -> Option<(usize, Answer)> {
    records
        .iter()
        .enumerate()
        .find_map(|(index, record)| match record {
            Record::Resolved {
                request: answered,
                answer,
                ..
            } if *answered == request => Some((index, answer.clone())),
            _ => None,
        })
}

fn tool_result_position(records: &[Record], tool: &str) -> Option<usize> {
    records.iter().position(|record| {
        matches!(record, Record::ToolResult(entry)
            if matches!(&entry.kind, EntryKind::ToolResult { name, .. } if &**name == tool))
    })
}

/// A fast answer cannot outrun the actor's fold: the resolution journals and
/// no orphaned question survives the turn.
#[tokio::test]
async fn a_fast_answer_cannot_outrun_the_asked_fold() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let request = next_opened(&mut session.subscription).await?;
    session.agent.answer(request, Answer::Approve).await?;
    let (dump, _) = finish_turn(&mut session).await?;
    assert!(dump.contains("the probe ran"), "{dump}");
    let id = session.agent.view(dal_core::PageReq::default())?.session.id;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    let (resolved_at, _) = journaled_answer(&records, request)
        .ok_or("the fast answer left no journaled resolution")?;
    let result_at = tool_result_position(&records, "fixture__probe")
        .ok_or("the approved call left no tool result")?;
    assert!(
        resolved_at < result_at,
        "the resolution must journal before its effect"
    );
    let agent = session
        .host
        .open(
            SessionRef::Resume {
                key: id.to_string().into(),
                workspace: Workspace::new(session.tmp.path().join("w"))?,
            },
            ClientId::new("probe"),
        )
        .await?;
    assert!(
        agent.view(dal_core::PageReq::default())?.open.is_empty(),
        "an orphaned question survived the turn"
    );
    session.host.close(id).await?;
    Ok(())
}

/// A session-level approval replays after resume: the next matching call
/// runs without opening a new request.
#[tokio::test]
async fn a_session_approval_survives_resume() -> TestResult {
    let mut session = start_session(2).await?;
    submit_prompt(&session.agent).await?;
    let request = next_opened(&mut session.subscription).await?;
    session
        .agent
        .answer(request, Answer::ApproveForSession)
        .await?;
    let (dump, _) = finish_turn(&mut session).await?;
    assert!(dump.contains("the probe ran"), "{dump}");
    let id = session.agent.view(dal_core::PageReq::default())?.session.id;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert!(
        records.iter().any(|record| matches!(
            record,
            Record::AllowAlways { tool, .. } if &**tool == "fixture__probe"
        )),
        "no durable session approval was journaled"
    );
    let agent = session
        .host
        .open(
            SessionRef::Resume {
                key: id.to_string().into(),
                workspace: Workspace::new(session.tmp.path().join("w"))?,
            },
            ClientId::new("probe"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run the probe".into(),
            }],
        })
        .await?;
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the second turn ended")?;
        if let Delivery::Update(update) = &delivery {
            match &update.kind {
                UpdateKind::RequestOpened(opened) if matches!(&opened.question, dal_core::Question::Approval { tool, .. } if &**tool == "fixture__probe") =>
                {
                    return Err(
                        "the resumed session asked again despite the session approval".into(),
                    );
                }
                UpdateKind::TurnEnded { .. } => break,
                _ => {}
            }
        }
    }
    let view = agent.view(dal_core::PageReq::default())?;
    assert!(
        format!("{view:?}").contains("the probe ran"),
        "the resumed call did not run"
    );
    session.host.close(id).await?;
    Ok(())
}
