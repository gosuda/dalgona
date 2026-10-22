//! Approval durability at the dispatch boundary: the actor folds an
//! approval before any client can answer it, session-level approvals replay
//! after resume, and a failed journal append runs no provider effect.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, ExportSpec, Extension, ExtensionBuilder, Hook, HookCx, HookError,
    RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
};
use dal_agent::{
    Agent, AgentError, Delivery, Env, Host, Product, SessionRef, Subscription, ToolError,
};
use dal_core::ext::{BeforeTurn, ExportId, ExportKind, OpSet};
use dal_core::{
    Answer, ApprovalMode, CancelScope, ClientId, Command, Config, ConfigProduct, EntryKind, Expect,
    ModelInfo, Name, Part, Question, RawJson, Record, RequestId, ResolutionCause, Save, SessionId,
    Stop, ToolClass, ToolSpec, TurnId, UpdateKind, Visibility, Workspace,
};
use dal_store::Store;
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(20);

type TestResult = Result<(), Box<dyn std::error::Error>>;

const STEP_CALL: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"probe-call\",\"name\":\"fixture__probe\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"probe-call\",\"name\":\"fixture__probe\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}]}\n";
const STEP_END: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"done\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct ApprovalProbe {
    name: Name,
    spec: Arc<ToolSpec>,
    class: ToolClass,
}

impl Tool for ApprovalProbe {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(self.class.clone())
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
    let mut script = String::new();
    for _ in 0..turns {
        script.push_str(STEP_CALL);
        script.push_str(STEP_END);
    }
    start_scripted(&script, vec![probe_extension()?]).await
}

/// The class of the execution probe: it asks in every mode but `all`.
fn exec_class() -> ToolClass {
    ToolClass::Exec {
        read_only: false,
        grant: None,
    }
}

/// One approval probe of `class` named `fixture__<local>`, with its export.
fn probe_tool(
    local: &str,
    class: ToolClass,
) -> Result<(Arc<ApprovalProbe>, ExportSpec), Box<dyn std::error::Error>> {
    let name = format!("fixture__{local}");
    let probe = Arc::new(ApprovalProbe {
        name: Name::parse(&name)?,
        spec: Arc::new(ToolSpec {
            name: Name::parse(&name)?,
            description: "approval probe tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        class,
    });
    let export = ExportSpec {
        id: ExportId {
            plugin: Name::parse("fixture")?,
            kind: ExportKind::Tool,
            local: Name::parse(local)?,
        },
        uses: OpSet::EMPTY,
        input: RawJson::parse(r#"{"type":"object"}"#)?,
        description: "approval probe export".into(),
    };
    Ok((probe, export))
}

/// The approval probe's extension: one tool that asks about itself.
fn probe_extension() -> Result<Extension, Box<dyn std::error::Error>> {
    let (probe, export) = probe_tool("probe", exec_class())?;
    Ok(
        ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
            .script_tool(probe, Visibility::Model, export)
            .build()?,
    )
}

/// The mode probes: an execution tool that asks unless the mode is `all`, and
/// an edit tool that asks only in `ask`, so each approval mode decides the
/// two differently.
fn mode_extension() -> Result<Extension, Box<dyn std::error::Error>> {
    let (run, run_export) = probe_tool("probe", exec_class())?;
    let (patch, patch_export) = probe_tool("patch", ToolClass::Patch)?;
    Ok(
        ExtensionBuilder::new("fixture", "0.1.0", dal_core::ServiceSet::EMPTY)?
            .script_tool(run, Visibility::Model, run_export)
            .script_tool(patch, Visibility::Model, patch_export)
            .build()?,
    )
}

/// Starts one host that serves `script` through `extensions`, with an
/// answering client attached.
async fn start_scripted(
    script: &str,
    extensions: Vec<Extension>,
) -> Result<Session, Box<dyn std::error::Error>> {
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
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
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

fn journaled_answer(
    records: &[Record],
    request: dal_core::RequestId,
) -> Option<(usize, Answer, Option<ResolutionCause>)> {
    records
        .iter()
        .enumerate()
        .find_map(|(index, record)| match record {
            Record::Resolved {
                request: answered,
                answer,
                cause,
                ..
            } if *answered == request => Some((index, answer.clone(), *cause)),
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
    let (resolved_at, answer, cause) = journaled_answer(&records, request)
        .ok_or("the fast answer left no journaled resolution")?;
    assert!(matches!(answer, Answer::Approve), "{answer:?}");
    assert_eq!(
        cause,
        Some(ResolutionCause::Answered),
        "the fast answer journals as Answered"
    );
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

/// One scripted provider step that calls the held tool.
fn held_call() -> String {
    STEP_CALL
        .replace("probe-call", "held-call")
        .replace("fixture__probe", "fixture__held")
}

/// Blocks inside its call until the test opens `gate`, then asks for
/// approval of itself. It ignores cancellation, so a cancelled turn stays
/// settling while the call is held and the question opens under the ended
/// turn.
struct HeldApproval {
    name: Name,
    spec: Arc<ToolSpec>,
    entered: Arc<Notify>,
    gate: Arc<Notify>,
}

impl Tool for HeldApproval {
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
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        Box::pin(async move {
            entered.notify_one();
            gate.notified().await;
            let preview = dal_core::Preview {
                title: "held-approval".into(),
                body: "printf late".into(),
                digest: None,
            };
            match cx.authorize(preview).await {
                Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("the held call ran"))),
                Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
            }
        })
    }
}

/// Asks one question while the second prompt's opening runs, which keeps
/// that opening suspended in its pump until the question is answered.
struct AskOnOpening;

impl Hook<BeforeTurn, Option<String>> for AskOnOpening {
    fn call(
        &self,
        event: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        Box::pin(async move {
            if event.text.as_ref() != "second prompt" {
                return Ok(None);
            }
            let question = Question::Text {
                prompt: "hold the opening".into(),
                placeholder: None,
            };
            let _ = cx.services.ask(&cx.caller, question).await;
            Ok(None)
        })
    }
}

/// The extension of the nested-settle case: the held tool and the asking
/// opening hook.
fn held_extension(
    entered: &Arc<Notify>,
    gate: &Arc<Notify>,
) -> Result<Extension, Box<dyn std::error::Error>> {
    let held = Arc::new(HeldApproval {
        name: Name::parse("fixture__held")?,
        spec: Arc::new(ToolSpec {
            name: Name::parse("fixture__held")?,
            description: "held approval tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#)?,
            grammar: None,
        }),
        entered: Arc::clone(entered),
        gate: Arc::clone(gate),
    });
    Ok(ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        dal_core::ServiceSet::from_names(["ask"])?,
    )?
    .script_tool(
        held,
        Visibility::Model,
        ExportSpec {
            id: ExportId {
                plugin: Name::parse("fixture")?,
                kind: ExportKind::Tool,
                local: Name::parse("held")?,
            },
            uses: OpSet::EMPTY,
            input: RawJson::parse(r#"{"type":"object"}"#)?,
            description: "held approval export".into(),
        },
    )
    .on_before_turn(AskOnOpening)
    .build()?)
}

/// Submits one prompt and returns the turn it opened.
async fn submit_text(agent: &Agent, text: &str) -> Result<TurnId, Box<dyn std::error::Error>> {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    match reply {
        dal_core::Reply::Accepted { turn, .. } => Ok(turn),
        other => Err(format!("prompt not accepted: {other:?}").into()),
    }
}

/// The id of the next opened request whose question `wanted` accepts.
async fn next_question(
    subscription: &mut Subscription,
    wanted: impl Fn(&Question) -> bool,
) -> Result<RequestId, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the question opened")?;
        if let Delivery::Update(update) = &delivery
            && let UpdateKind::RequestOpened(request) = &update.kind
            && wanted(&request.question)
        {
            return Ok(request.id);
        }
    }
    Err("the wanted question never opened".into())
}

/// A failing nested append never settles a claim an outer frame already
/// landed.
///
/// A turn is cancelled while its tool call is held, so the driver has not
/// confirmed the stop and the actor keeps the turn settling. A second prompt
/// accepted meanwhile parks its opening. The released call asks for approval
/// under the ended turn; the stop confirmation journals that leftover
/// question as `Cancelled` and, in the same frame, drives the parked opening.
/// The opening's hook asks a question and stays suspended; answering it with
/// a journal that refuses the write runs a nested frame that fails. That
/// failure may recycle only its own claim: the `Cancelled` claim already
/// holds its one terminal record and must stay resolved.
#[tokio::test]
async fn a_nested_append_failure_never_recycles_an_outer_landed_claim() -> TestResult {
    let entered = Arc::new(Notify::new());
    let gate = Arc::new(Notify::new());
    let script = format!("{}{STEP_END}", held_call());
    let mut session = start_scripted(&script, vec![held_extension(&entered, &gate)?]).await?;
    let first = submit_text(&session.agent, "first prompt").await?;
    tokio::time::timeout(WAIT, entered.notified()).await?;
    let cancel = session
        .agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(first),
        })
        .await?;
    assert!(
        matches!(cancel, dal_core::Reply::Done(_)),
        "cancel not done: {cancel:?}"
    );
    submit_text(&session.agent, "second prompt").await?;
    gate.notify_one();
    let late = next_question(&mut session.subscription, |question| {
        matches!(question, Question::Approval { tool, .. } if &**tool == "fixture__held")
    })
    .await?;
    let asked = next_question(&mut session.subscription, |question| {
        matches!(question, Question::Text { prompt, .. } if &**prompt == "hold the opening")
    })
    .await?;
    let id = session.agent.session();
    let store = Store::new(
        session.tmp.path().join("data"),
        Workspace::new(session.tmp.path().join("w"))?,
        dal_core::Product::Dal,
    );
    store.arm_append_sync_fault(id);
    let refused = session
        .agent
        .answer(asked, Answer::Value(RawJson::parse("\"too late\"")?))
        .await
        .err()
        .ok_or("the unjournaled answer was acknowledged")?;
    assert!(
        matches!(refused, AgentError::PersistenceFailed { .. }),
        "a refused journal append must surface the persistence failure, not a validation rejection: {refused:?}"
    );
    assert!(
        refused.to_string().contains("journal"),
        "unexpected failure: {refused}"
    );
    let open = session.agent.view(dal_core::PageReq::default())?.open;
    assert!(
        !open.iter().any(|request| request.id == late),
        "the landed claim became an open question again"
    );
    let retry = session.agent.answer(late, Answer::Decline).await;
    assert!(
        matches!(retry, Err(AgentError::AlreadyResolved { .. })),
        "a retry met something other than the landed claim: {retry:?}"
    );
    session.host.close(id).await?;
    let records = journal_records(&session.tmp, id).await?;
    let terminals = records
        .iter()
        .filter(|record| matches!(record, Record::Resolved { request, .. } if *request == late))
        .count();
    assert_eq!(
        terminals, 1,
        "the late approval holds exactly one terminal record"
    );
    assert!(
        matches!(
            journaled_answer(&records, late),
            Some((_, Answer::Cancel, Some(ResolutionCause::Cancelled)))
        ),
        "the late approval's terminal record is the stop's Cancelled"
    );
    Ok(())
}

/// One scripted provider step that calls `tool` under call id `id`.
fn call_step(id: &str, tool: &str) -> String {
    STEP_CALL
        .replace("probe-call", id)
        .replace("fixture__probe", tool)
}

/// Submits one approval-mode change for this session only.
async fn set_approval(agent: &Agent, mode: ApprovalMode) -> Result<(), AgentError> {
    agent
        .submit(Command::SetApproval {
            mode,
            save: Save::SessionOnly,
        })
        .await
        .map(|_| ())
}

/// What one update means to a waiter for an approval about `tool`: the
/// request id when it is that approval, an error for any other request or an
/// ended turn, nothing for unrelated traffic.
fn approval_verdict(
    kind: &UpdateKind,
    tool: &str,
) -> Option<Result<RequestId, Box<dyn std::error::Error>>> {
    match kind {
        UpdateKind::RequestOpened(request) => Some(match &request.question {
            Question::Approval { tool: asked, .. } if &**asked == tool => Ok(request.id),
            other => Err(format!("unexpected request while waiting for {tool}: {other:?}").into()),
        }),
        UpdateKind::TurnEnded { .. } => Some(Err(format!(
            "the turn ended without asking about {tool}"
        )
        .into())),
        _ => None,
    }
}

/// The id of the next approval about `tool`. Any other approval, or the turn
/// ending first, means the live mode decided differently than expected.
async fn next_approval_for(
    subscription: &mut Subscription,
    tool: &str,
) -> Result<RequestId, Box<dyn std::error::Error>> {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the approval opened")?;
        if let Delivery::Update(update) = &delivery
            && let Some(verdict) = approval_verdict(&update.kind, tool)
        {
            return verdict;
        }
    }
    Err(format!("no approval opened for {tool}").into())
}

/// Waits for the end of the turn and fails on any approval: the live mode
/// let every call run unasked.
async fn turn_ends_unasked(subscription: &mut Subscription) -> TestResult {
    for _ in 0..200 {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await?
            .ok_or("the session closed before the turn ended")?;
        let Delivery::Update(update) = &delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                return Err(format!("the live mode asked: {:?}", request.question).into());
            }
            UpdateKind::TurnEnded { .. } => return Ok(()),
            _ => {}
        }
    }
    Err("the turn never ended".into())
}

/// Every approval mode the journal records, in journal order.
fn recorded_modes(records: &[Record]) -> Vec<ApprovalMode> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::Approval(entry) => match &entry.kind {
                EntryKind::Approval { mode } => Some(*mode),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A refused approval-mode change leaves the live mode, and the next
/// approval decision, at the prior durable mode.
///
/// The session records `edits` first, so the prior durable mode is neither
/// the starting mode nor the refused one: `edits` runs the patch tool unasked
/// and still asks about execution. The journal refuses the change to `all`
/// while a turn waits on its first approval; the turn's later calls must keep
/// deciding under `edits`. A reopened session restores `edits`, and the retried
/// change then lands and takes effect.
#[tokio::test]
async fn a_failed_approval_change_leaves_the_prior_durable_mode() -> TestResult {
    let script = [
        call_step("c1", "fixture__probe"),
        call_step("c2", "fixture__patch"),
        call_step("c3", "fixture__probe"),
        STEP_END.to_owned(),
        call_step("c4", "fixture__probe"),
        STEP_END.to_owned(),
        call_step("c5", "fixture__probe"),
        STEP_END.to_owned(),
    ]
    .concat();
    let mut session = start_scripted(&script, vec![mode_extension()?]).await?;
    let id = session.agent.session();
    set_approval(&session.agent, ApprovalMode::Edits).await?;
    submit_prompt(&session.agent).await?;
    let first = next_approval_for(&mut session.subscription, "fixture__probe").await?;
    let store = Store::new(
        session.tmp.path().join("data"),
        Workspace::new(session.tmp.path().join("w"))?,
        dal_core::Product::Dal,
    );
    store.arm_append_write_fault(id, 0);
    let refused = set_approval(&session.agent, ApprovalMode::All)
        .await
        .err()
        .ok_or("the unjournaled approval change was acknowledged")?;
    assert!(
        matches!(refused, AgentError::PersistenceFailed { .. }),
        "a refused journal append must surface the persistence failure, not a validation rejection: {refused:?}"
    );
    assert!(
        refused.to_string().contains("journal"),
        "unexpected failure: {refused}"
    );
    store.arm_append_write_fault(id, usize::MAX);
    session.agent.answer(first, Answer::Approve).await?;
    let second = next_approval_for(&mut session.subscription, "fixture__probe").await?;
    session.agent.answer(second, Answer::Decline).await?;
    turn_ends_unasked(&mut session.subscription).await?;
    session.host.close(id).await?;
    let modes = recorded_modes(&journal_records(&session.tmp, id).await?);
    assert_eq!(
        modes.last(),
        Some(&ApprovalMode::Edits),
        "the recorded mode is still edits: {modes:?}"
    );
    assert!(
        !modes.contains(&ApprovalMode::All),
        "the refused change left a record: {modes:?}"
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
    submit_prompt(&agent).await?;
    let third = next_approval_for(&mut subscription, "fixture__probe").await?;
    agent.answer(third, Answer::Decline).await?;
    turn_ends_unasked(&mut subscription).await?;
    set_approval(&agent, ApprovalMode::All).await?;
    submit_prompt(&agent).await?;
    turn_ends_unasked(&mut subscription).await?;
    // A genuine validation failure through the same agent surface stays a
    // rejection: answering an unknown request touches nothing, so it must
    // not wear the persistence failure. The probe runs while the session is
    // still open, so it observes validation, never the closed surface.
    let genuine = agent
        .answer(RequestId::new_v7(), Answer::Approve)
        .await
        .err()
        .ok_or("an answer to an unknown request was acknowledged")?;
    assert!(
        matches!(genuine, AgentError::Invalid(_)),
        "a validation rejection must stay Invalid: {genuine:?}"
    );
    session.host.close(id).await?;
    let modes = recorded_modes(&journal_records(&session.tmp, id).await?);
    assert_eq!(
        modes.last(),
        Some(&ApprovalMode::All),
        "the retried change landed: {modes:?}"
    );
    Ok(())
}
