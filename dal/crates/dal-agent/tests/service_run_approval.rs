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
    Answer, ClientId, Command, EntryKind, Expect, ModelInfo, Name, Part, RawJson, Record,
    RunRequest, SessionId, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};
use dal_store::Store;

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
    host: Host,
    agent: Agent,
    subscription: Option<Subscription>,
    listener: Subscription,
    tmp: tempfile::TempDir,
}

/// Appends one line to the TOML user config under construction.
fn write_toml_line(into: &mut String, line: &str) {
    into.push_str(line);
    into.push('\n');
}

/// Builds the host, opens a session, and submits the probe prompt.
async fn host_with_probe(
    approval: Option<&str>,
) -> Result<(Host, Agent, tempfile::TempDir), Box<dyn std::error::Error>> {
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
    Ok((host, agent, tmp))
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
    let (host, agent, tmp) = host_with_probe(None).await?;
    let subscription = agent.subscribe(None)?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        host,
        agent,
        subscription: Some(subscription),
        listener,
        tmp,
    })
}

/// Default `ask` mode with only a listener: nobody can answer.
async fn start_ask_headless() -> Result<Session, Box<dyn std::error::Error>> {
    let (host, agent, tmp) = host_with_probe(None).await?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        host,
        agent,
        subscription: None,
        listener,
        tmp,
    })
}

/// `all` mode with only a listener.
async fn start_all_headless() -> Result<Session, Box<dyn std::error::Error>> {
    let (host, agent, tmp) = host_with_probe(Some("all")).await?;
    submit_probe(&agent).await?;
    let listener = agent.subscribe_listen(None)?;
    Ok(Session {
        host,
        agent,
        subscription: None,
        listener,
        tmp,
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
            if matches!(&entry.kind, EntryKind::ToolResult { name, .. } if &**name == "fixture__do_run"))
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

/// Reads updates on the answering subscription until `request`'s
/// resolution is broadcast. One bounded wait: nothing at HEAD broadcasts,
/// so an unbounded read would stall the whole RED run.
async fn wait_for_resolved(
    subscription: &mut Option<Subscription>,
    request: dal_core::RequestId,
) -> Result<Answer, Box<dyn std::error::Error>> {
    let Some(subscription) = subscription.as_mut() else {
        return Err("no answerer attached".into());
    };
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let delivery = tokio::time::timeout(WAIT, subscription.next())
                .await?
                .ok_or("the session closed before the resolution broadcast")?;
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

/// An approved run journals its resolution before the settled result that
/// carries the process output, and a resumed session still shows the record.
#[tokio::test]
async fn an_approved_run_journals_the_answer_before_the_process_runs() -> TestResult {
    let mut session = start_ask_answered().await?;
    let request = next_run_request(&mut session).await?;
    session.agent.answer(request, Answer::Approve).await?;
    let dump = finish_turn(&mut session).await?;
    assert!(dump.contains("hello"), "the approved run ran: {dump}");
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
        return Err("the run result is not journaled".into());
    };
    assert!(
        resolved_at < effect_at,
        "the answer must land in the journal before the run it allowed"
    );
    resumed_shows(&session, id, request).await?;
    Ok(())
}

/// A declined run journals the decline before the denial result, and a
/// resumed session still shows the record.
#[tokio::test]
async fn a_declined_run_journals_the_answer_before_its_denial() -> TestResult {
    let mut session = start_ask_answered().await?;
    let request = next_run_request(&mut session).await?;
    session.agent.answer(request, Answer::Decline).await?;
    let dump = finish_turn(&mut session).await?;
    assert!(
        dump.contains("Permission denied: run was declined by probe."),
        "the denial names the decider: {dump}"
    );
    assert!(!dump.contains("hello"), "nothing ran: {dump}");
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

/// A run nobody answers broadcasts its fail-closed default and journals
/// exactly that record: nothing is published as resolved that the journal
/// does not hold.
#[tokio::test]
async fn a_run_nobody_answers_journals_the_default_it_broadcasts() -> TestResult {
    let mut session = start_ask_answered().await?;
    let request = next_run_request(&mut session).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    tokio::time::resume();
    let broadcast = wait_for_resolved(&mut session.subscription, request).await?;
    assert!(matches!(broadcast, Answer::Decline), "{broadcast:?}");
    let dump = finish_turn(&mut session).await?;
    assert!(
        dump.contains("run needed approval and no one answered within 300 s."),
        "the denial names the timeout: {dump}"
    );
    assert!(!dump.contains("hello"), "nothing ran: {dump}");
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
