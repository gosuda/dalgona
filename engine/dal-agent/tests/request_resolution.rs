//! Request terminal transitions at the Host boundary: one real terminal
//! cause per accepted published request, one waiter release, no false
//! durable claim, and no stuck admission across cancel/shutdown races.
//!
//! These tests drive a real `Host` with a scripted provider and real
//! subscriptions, asserting the durable journal through `dal_store`. They
//! cover what the unit levels cannot: the actor folding inbox claims to
//! journal records and the registry classifying before its lifecycle fires.
//!
//! Already covered elsewhere, so not repeated here:
//! - `approval_outcome.rs`: a decline reports as a decision by its client,
//!   and an unanswered approval broadcasts its fail-closed default text.
//! - `approval_durability.rs`: a fast answer cannot outrun the Asked fold,
//!   and a session approval survives resume without a new request.
//! - `answer_scope.rs`: each answerer role routes only its own kind.
//! - `broker/tests.rs`: cause provenance per constructor (`release` carries
//!   `Some`, `cancel` and sender drops carry `None`) and the per-class
//!   expiry probe (`TimedOut` vs `NoController`).
//! - `session.rs` inline tests: the 17-withdrawal bound and oldest-first
//!   cancel ordering at the inbox.

// No lint-expect attribute: `clippy::panic` fires only on bare `panic!`
// (sibling proof: `approval_outcome.rs` asserts freely with no header),
// and this file asserts via `assert!` plus `?` throughout.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{
    Agent, AgentError, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError,
};
use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, CancelScope, ClientId, Command, Config, ConfigProduct, Expect, ModelInfo, Name, Part,
    Preview, RawJson, Record, RequestId, ResolutionCause, SessionId, Stop, ToolClass, ToolSpec,
    TurnId, UpdateKind, Visibility, Workspace,
};
use dal_store::Store;

/// Bounds each real-clock wait in the resolution tests.
const WAIT: Duration = Duration::from_secs(120);

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

/// Starts one host serving `script`, with an answering client attached.
async fn start_scripted(script: String) -> Result<Session, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace_dir)?;
    let fixture = data.join("script.jsonl");
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

/// Starts one host whose script serves `turns` identical probe turns.
async fn start_session(turns: usize) -> Result<Session, Box<dyn std::error::Error>> {
    let mut script = String::new();
    for _ in 0..turns {
        script.push_str(STEP_CALL);
        script.push_str(STEP_END);
    }
    start_scripted(script).await
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

/// Reads updates until the running turn and its approval request both
/// surface, returning their ids.
async fn next_turn_and_request(
    subscription: &mut Subscription,
) -> Result<(TurnId, RequestId), Box<dyn std::error::Error>> {
    let mut turn = None;
    let mut request = None;
    for _ in 0..400 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the approval opened")?;
        if let Delivery::Update(update) = &delivery {
            match &update.kind {
                UpdateKind::TurnStarted { turn: started, .. } => turn = Some(*started),
                UpdateKind::RequestOpened(opened) => request = Some(opened.id),
                _ => {}
            }
        }
        if let (Some(turn), Some(request)) = (turn, request) {
            return Ok((turn, request));
        }
    }
    Err("no approval request opened".into())
}

/// Waits until the fold registered the request, so a racing cancel meets a
/// reported slot and the claim path is deterministic.
async fn wait_fold_registered(agent: &Agent, id: RequestId) -> TestResult {
    for _ in 0..200 {
        if agent
            .view(dal_core::PageReq::default())?
            .open
            .iter()
            .any(|open| open.id == id)
        {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    Err("the approval request did not enter the session view".into())
}

/// Reads updates until the turn ends, returning its id and stop reason.
async fn wait_turn_ended(
    subscription: &mut Subscription,
) -> Result<(TurnId, Stop), Box<dyn std::error::Error>> {
    for _ in 0..400 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        if let Delivery::Update(update) = &delivery
            && let UpdateKind::TurnEnded { turn, stop } = update.kind
        {
            return Ok((turn, stop));
        }
    }
    Err("the turn never ended".into())
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

/// Every journaled terminal record for `id`: its answer, default flag, and
/// cause. Exactly one entry means exactly one durable terminal outcome.
fn resolved_for(records: &[Record], id: RequestId) -> Vec<(Answer, bool, Option<ResolutionCause>)> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Resolved {
                request,
                answer,
                was_default,
                cause,
                ..
            } if *request == id => Some((answer.clone(), *was_default, *cause)),
            _ => None,
        })
        .collect()
}

/// The single-winner terminal transition: a turn cancelled while its
/// approval is open journals one `Cancelled` record, and the late answer is
/// rejected instead of overriding the winner or echoing a second record.
#[tokio::test]
async fn a_cancelled_turn_rejects_its_late_answer_with_one_cancelled_record() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let (turn, request) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, request).await?;
    session
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await?;
    let (ended, stop) = wait_turn_ended(&mut session.subscription).await?;
    assert_eq!(ended, turn, "the cancelled turn ended");
    assert_eq!(stop, Stop::Cancelled, "the turn ended cancelled");
    let late = session.agent.answer(request, Answer::Approve).await;
    assert!(
        matches!(late, Err(AgentError::AlreadyResolved { .. })),
        "the late answer must lose to the cancel: {late:?}"
    );
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, request),
        vec![(Answer::Cancel, false, Some(ResolutionCause::Cancelled))],
        "one durable Cancelled record, no Withdrawn, no echo duplicate"
    );
    Ok(())
}

/// Shutdown with an open request: the registry classifies the running turn
/// before its lifecycle fires, so the drain journals one `Cancelled`
/// resolution instead of a `Withdrawn` or a second record.
#[tokio::test]
async fn a_close_with_an_open_request_journals_one_cancelled_resolution() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let (_, request) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, request).await?;
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, request),
        vec![(Answer::Cancel, false, Some(ResolutionCause::Cancelled))],
        "one durable Cancelled record for the open request"
    );
    Ok(())
}

/// Retained admission after an abandoned ask: cancelling a turn with an open
/// approval settles that ask once, and the next turn's ask is still admitted
/// and settles durably. The script serves two calls positionally — the
/// cancelled first turn consumes only its call step — so the second turn
/// still opens its own approval.
#[tokio::test]
async fn an_abandoned_ask_frees_admission_for_a_later_durable_settlement() -> TestResult {
    let script = format!("{STEP_CALL}{STEP_CALL}{STEP_END}");
    let mut session = start_scripted(script).await?;
    submit_prompt(&session.agent).await?;
    let (first_turn, abandoned) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, abandoned).await?;
    session
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(first_turn),
        })
        .await?;
    let (ended, stop) = wait_turn_ended(&mut session.subscription).await?;
    assert_eq!(ended, first_turn, "the abandoned turn ended");
    assert_eq!(stop, Stop::Cancelled, "the abandoned turn ended cancelled");
    submit_prompt(&session.agent).await?;
    let (_, settled) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, settled).await?;
    session.agent.answer(settled, Answer::Approve).await?;
    let (_, stop) = wait_turn_ended(&mut session.subscription).await?;
    assert_ne!(stop, Stop::Cancelled, "the second turn ran to its own end");
    let dump = format!("{:?}", session.agent.view(dal_core::PageReq::default())?);
    assert!(
        dump.contains("the probe ran"),
        "the second approval ran: {dump}"
    );
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, abandoned),
        vec![(Answer::Cancel, false, Some(ResolutionCause::Cancelled))],
        "the abandoned ask settled exactly once"
    );
    assert_eq!(
        resolved_for(&records, settled),
        vec![(Answer::Approve, false, Some(ResolutionCause::Answered))],
        "the later ask settled durably after the abandonment"
    );
    Ok(())
}

/// No fake answer echo: a released client answer journals once, and the
/// dispatch echo of that settlement folds to no emit instead of a second
/// record.
#[tokio::test]
async fn an_answered_approval_echoes_no_second_record() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let (_, request) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, request).await?;
    session.agent.answer(request, Answer::Approve).await?;
    wait_turn_ended(&mut session.subscription).await?;
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, request),
        vec![(Answer::Approve, false, Some(ResolutionCause::Answered))],
        "one durable Answered record; the echo must not duplicate it"
    );
    Ok(())
}

/// An attended deadline journals `TimedOut` once: the answering subscriber
/// stays attached through the sweep, so the expiry keeps the timeout cause.
#[tokio::test]
async fn an_attended_deadline_journals_timed_out_once() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let (_, request) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, request).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();
    wait_turn_ended(&mut session.subscription).await?;
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, request),
        vec![(Answer::Decline, true, Some(ResolutionCause::TimedOut))],
        "one durable TimedOut default record"
    );
    Ok(())
}

/// No controller vs timeout: with no approval answerer attached at the
/// sweep, the same deadline journals `NoController` instead of `TimedOut`.
/// The answering subscription is dropped after the request opens — a dead
/// subscriber never counts as attached — while a listen-only subscription
/// observes the turn end without answering for anyone.
#[tokio::test]
async fn an_unattended_deadline_journals_no_controller_once() -> TestResult {
    let mut session = start_session(1).await?;
    submit_prompt(&session.agent).await?;
    let (_, request) = next_turn_and_request(&mut session.subscription).await?;
    wait_fold_registered(&session.agent, request).await?;
    let answering = std::mem::replace(
        &mut session.subscription,
        session.agent.subscribe_listen(None)?,
    );
    drop(answering);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();
    wait_turn_ended(&mut session.subscription).await?;
    let id = session.agent.session();
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    assert_eq!(
        resolved_for(&records, request),
        vec![(Answer::Decline, true, Some(ResolutionCause::NoController))],
        "one durable NoController default record"
    );
    Ok(())
}
