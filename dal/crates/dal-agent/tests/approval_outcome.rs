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
    Answer, ClientId, Command, Config, ConfigProduct, EntryKind, Expect, ModelInfo, Name, Part,
    Preview, RawJson, Record, SessionId, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};
use dal_store::Store;

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
    host: Host,
    agent: Agent,
    subscription: Subscription,
    tmp: tempfile::TempDir,
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
        host,
        agent,
        subscription,
        tmp,
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

/// Reads one closed session's journal records from disk.
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

/// The journaled resolution of `request` with its record position.
fn journaled_resolution(
    records: &[Record],
    request: dal_core::RequestId,
) -> Option<(usize, Answer, ClientId, bool)> {
    records
        .iter()
        .enumerate()
        .find_map(|(index, record)| match record {
            Record::Resolved {
                request: answered,
                answer,
                by,
                was_default,
                ..
            } if *answered == request => Some((index, answer.clone(), by.clone(), *was_default)),
            _ => None,
        })
}

/// Position of the probe tool's settled result record.
fn probe_result(records: &[Record]) -> Option<usize> {
    records.iter().position(|record| {
        matches!(record, Record::ToolResult(entry)
            if matches!(&entry.kind, EntryKind::ToolResult { name, .. } if &**name == "fixture__probe"))
    })
}

/// Requires the closed session to resume with `request`'s resolution still
/// journaled, then closes it again.
async fn resumed_shows(
    session: &Session,
    id: SessionId,
    request: dal_core::RequestId,
) -> TestResult {
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
    agent.view(dal_core::PageReq::default())?;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert!(
        journaled_resolution(&records, request).is_some(),
        "the resumed session lost the resolution record"
    );
    Ok(())
}

/// The answer the broadcast carries must sit in the journal before the
/// settled result that consumed it, and a resumed session still shows it.
#[tokio::test]
async fn an_approved_tool_call_journals_the_answer_before_its_effect() -> TestResult {
    let mut session = start_turn().await?;
    let request = next_request(&session.agent, &mut session.subscription, WAIT).await?;
    session.agent.answer(request, Answer::Approve).await?;
    let dump = finish_turn(&mut session, WAIT).await?;
    assert!(dump.contains("the probe ran"), "{dump}");
    let id = session.agent.view(dal_core::PageReq::default())?.session.id;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    let Some((resolved_at, answer, by, was_default)) = journaled_resolution(&records, request)
    else {
        return Err("the approved answer is not journaled".into());
    };
    assert!(matches!(answer, Answer::Approve), "{answer:?}");
    assert_eq!(by.as_str(), "probe");
    assert!(!was_default);
    let Some(effect_at) = probe_result(&records) else {
        return Err("the probe result is not journaled".into());
    };
    assert!(
        resolved_at < effect_at,
        "the answer must land in the journal before the result that used it"
    );
    resumed_shows(&session, id, request).await?;
    Ok(())
}

/// A declined tool call journals the decline before the denial result,
/// and a resumed session still shows the record.
#[tokio::test]
async fn a_declined_tool_call_journals_the_answer_before_its_denial() -> TestResult {
    let mut session = start_turn().await?;
    let request = next_request(&session.agent, &mut session.subscription, WAIT).await?;
    session.agent.answer(request, Answer::Decline).await?;
    let dump = finish_turn(&mut session, WAIT).await?;
    assert!(dump.contains("was declined by probe"), "{dump}");
    assert!(!dump.contains("the probe ran"), "{dump}");
    let id = session.agent.view(dal_core::PageReq::default())?.session.id;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    let Some((resolved_at, answer, by, was_default)) = journaled_resolution(&records, request)
    else {
        return Err("the declined answer is not journaled".into());
    };
    assert!(matches!(answer, Answer::Decline), "{answer:?}");
    assert_eq!(by.as_str(), "probe");
    assert!(!was_default);
    let Some(effect_at) = probe_result(&records) else {
        return Err("the denial result is not journaled".into());
    };
    assert!(
        resolved_at < effect_at,
        "the decline must land in the journal before the denial it produced"
    );
    resumed_shows(&session, id, request).await?;
    Ok(())
}

/// An approval nobody answers broadcasts its fail-closed default and
/// journals exactly that record: nothing is published as resolved that
/// the journal does not hold.
#[tokio::test]
async fn an_unanswered_tool_approval_journals_the_default_it_broadcasts() -> TestResult {
    let mut session = start_turn().await?;
    let request = next_request(&session.agent, &mut session.subscription, WAIT).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    let broadcast = wait_for_resolved(&mut session.subscription, request).await?;
    assert!(matches!(broadcast, Answer::Decline), "{broadcast:?}");
    let dump = finish_turn(&mut session, WAIT).await?;
    assert!(dump.contains("no one answered within 300 s."), "{dump}");
    assert!(!dump.contains("the probe ran"), "{dump}");
    let id = session.agent.view(dal_core::PageReq::default())?.session.id;
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    let Some((_, answer, by, was_default)) = journaled_resolution(&records, request) else {
        return Err("the broadcast default is not journaled".into());
    };
    assert!(matches!(answer, Answer::Decline), "{answer:?}");
    assert_eq!(by.as_str(), "core");
    assert!(was_default);
    Ok(())
}

/// Reads updates until `request`'s resolution is broadcast. One bounded
/// wait: nothing at HEAD broadcasts, so an unbounded read would stall the
/// whole RED run.
async fn wait_for_resolved(
    subscription: &mut Subscription,
    request: dal_core::RequestId,
) -> Result<Answer, Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let delivery = next_delivery(subscription, WAIT).await?;
            if let Delivery::Update(update) = &delivery
                && let UpdateKind::RequestResolved { id, answer, .. } = &update.kind
                && *id == request
            {
                return Ok(answer.clone());
            }
        }
    })
    .await
    .map_err(|_| "no resolution broadcast arrived within 60 s")?
}
