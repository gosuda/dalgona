#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! Mailbox receipts must rest on journal durability.
//!
//! `Actor::on_mail` computed `Delivered`/`Buffered` and answered the sender
//! before (and regardless of) the `Record::Mail` append outcome, so a failed
//! journal write was reported as a successful receipt. These tests force a
//! real append failure through `dal_store`'s sync-fault facility and demand
//! the typed error instead of a receipt, while success controls prove the
//! durable path still answers with its receipt.
//!
//! The fault is `Store::arm_append_sync_fault`: every batch byte lands, then
//! the sync fails and the store rolls the batch back. The fault installs on
//! the session's journal handle at the first record-carrying batch after
//! arming and stays armed for that handle's remaining life, so each faulted
//! test pauses its probe tool before the send (no batch can run while the
//! turn waits on the gate), arms the fault, releases the send, and asserts
//! on the recorded reply alone — the later reply-step batch is expected to
//! fail too and never publishes `TurnEnded`. The missing-recipient and
//! `Full` contracts keep their receipts untouched: `Gone` is refused before
//! any journal work and `Full` skips the append by contract.
//!
//! The faulted-`Steer` test is the ordering proof. Its probe parks after the
//! reply, so no tool settle or boundary reaches the fold, `pop_steer` cannot
//! drain the queue, and `view().stats.steers_queued` reads the send's own
//! effect: 1 while `on_mail` steps before appending, 0 once the append leads.
//! No public API change is required: the private
//! `MailRequest::Send` reply channel gains the typed error and
//! `Backend::agent_send` maps it to the existing `ServiceError` future
//! result.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    ArgError, BoxFuture, Extension, ExtensionBuilder, RawValue, Tool, ToolCall, ToolCx,
    ToolOutcome, ToolOutput,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, ServiceError, SessionRef, Subscription};
use dal_core::{
    AgentsOp, AgentsReply, CallId, ClientId, Command, Config, ConfigProduct, Expect,
    MAILBOX_WAITING_LIMIT, MailMode, ModelInfo, Name, Part, RawJson, Receipt, Record, Service,
    ServiceSet, ToolClass, ToolSpec, UpdateKind, Visibility, Workspace,
};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(30);

const USAGE: &str = "{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}";

/// One scripted provider request that runs the probe tool and pauses.
fn tool_step(tool: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c-{tool}\",\"name\":\"{tool}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c-{tool}\",\"name\":\"{tool}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// One scripted provider request that ends its turn.
fn end_turn() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"done\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{USAGE},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// What one `AgentsOp::Send` answered, kept typed so assertions match the
/// variant and its fields instead of a `Debug` rendering.
type Reply = Result<AgentsReply, ServiceError>;

/// The reply value of a send that answered with `receipt`.
fn delivered(receipt: Receipt) -> AgentsReply {
    AgentsReply::Delivered(receipt)
}

/// A host and its workspace, backed by a temporary data root.
struct Live {
    host: Host,
    workspace: Workspace,
    tmp: tempfile::TempDir,
}

fn fixture_env(tmp: &std::path::Path, cwd: std::path::PathBuf) -> Env {
    Env {
        vars: BTreeMap::from([
            (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
            (OsString::from("HOME"), OsString::from(tmp.join("home"))),
            (
                OsString::from("XDG_CACHE_HOME"),
                OsString::from(tmp.join("cache")),
            ),
        ]),
        cwd,
        sandbox_helper: None,
    }
}

/// Builds the host with the given scripted steps: every host session shares
/// one scripted queue, so each test passes exactly the steps its own
/// sessions consume, in order, each ending in a terminal `Stop`.
async fn host_with_steps(extension: Extension, script: String) -> Live {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, script).expect("script fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(config_text.as_str())).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = fixture_env(tmp.path(), workspace_dir.clone());
    let host = Host::start(product, config, env).await.expect("host");
    let workspace = Workspace::new(workspace_dir).expect("workspace value");
    Live {
        host,
        workspace,
        tmp,
    }
}

async fn open_root(live: &Live) -> Agent {
    live.host
        .open(
            SessionRef::New {
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("mailbox-test"),
        )
        .await
        .expect("root session")
}

async fn open_child(live: &Live, parent: dal_core::SessionId, call: &str) -> Agent {
    live.host
        .open(
            SessionRef::Child {
                parent,
                call: CallId::new(call),
                workspace: live.workspace.clone(),
                name: None,
            },
            ClientId::new("mailbox-test"),
        )
        .await
        .expect("child session")
}

async fn turn_ended(subscription: &mut Subscription) {
    // A timed-out drain leaves the assertions to fail on the recorded
    // outcome.
    let _ = tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return;
            }
        }
    })
    .await;
}

/// Waits until the probe has recorded exactly one reply. The faulted tests
/// use this instead of `turn_ended`: the armed fault stays on the journal
/// handle, the reply-step batch fails, and `TurnEnded` never publishes.
async fn one_reply(seen: &Mutex<Vec<Reply>>) {
    tokio::time::timeout(WAIT, async {
        loop {
            if seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
                == 1
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the probe to record exactly one reply");
}

/// The session id the probe tool sends to, staged by the test after the
/// session opens (`ToolCx` does not expose it).
type Target = Arc<Mutex<Option<dal_core::SessionId>>>;

/// Sets the shared send target and returns the tool's view of it.
fn target_cell(cell: &Target, id: dal_core::SessionId) {
    *cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(id);
}

fn target_of(cell: &Target) -> dal_core::SessionId {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .expect("the test set its mail target")
}

/// A tool that pauses before its send until the test releases it, then
/// sends itself one message and reports the reply. The pre-send pause keeps
/// the turn running with no batch in flight, so a fault armed while the
/// tool waits can only fire on the send's own mail batch. Held tools then
/// park after the reply until `close` cancels them: the tool call stays in
/// flight, so no fold boundary can drain the steer the send queued before
/// the test reads the fold snapshot.
struct SelfSend {
    name: Name,
    spec: Arc<ToolSpec>,
    target: Target,
    mode: Arc<Mutex<MailMode>>,
    seen: Arc<Mutex<Vec<Reply>>>,
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl Tool for SelfSend {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let target = Arc::clone(&self.target);
        let mode = Arc::clone(&self.mode);
        let seen = Arc::clone(&self.seen);
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some((entered, gate)) = &hold {
                entered.notify_one();
                tokio::select! {
                    () = gate.notified() => {}
                    () = cx.cancel().cancelled() => return ToolOutcome::Interrupted,
                }
            }
            let to = target_of(&target);
            let mode = *mode
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let reply = cx
                .services()
                .agents(
                    cx.caller(),
                    AgentsOp::Send {
                        to,
                        text: "probe".into(),
                        mode,
                        reply_to: None,
                    },
                )
                .await;
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(reply);
            if hold.is_some() {
                cx.cancel().cancelled().await;
                return ToolOutcome::Interrupted;
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

fn self_send_extension(
    tool_name: &str,
    target: Target,
    mode: MailMode,
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
) -> (Arc<Mutex<Vec<Reply>>>, Extension) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let name = Name::parse(tool_name).expect("tool name");
    let extension = ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        ServiceSet::from_names(["agents"]).expect("agents service"),
    )
    .expect("builder")
    .tool(
        Arc::new(SelfSend {
            name: name.clone(),
            spec: Arc::new(ToolSpec {
                name,
                description: "mailbox durability probe".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                grammar: None,
            }),
            target,
            mode: Arc::new(Mutex::new(mode)),
            seen: Arc::clone(&seen),
            hold,
        }),
        Visibility::Model,
    )
    .build()
    .expect("probe extension");
    (seen, extension)
}

fn probe_tool_name(suffix: &str) -> String {
    format!("fixture__{suffix}_probe")
}

async fn run_prompt(agent: &Agent, text: &str) -> Subscription {
    let subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("prompt");
    subscription
}

async fn warm(agent: &Agent) {
    let mut subscription = run_prompt(agent, "warm the journal").await;
    turn_ended(&mut subscription).await;
}

async fn close_all(host: &Host, sessions: &[dal_core::SessionId]) {
    for session in sessions {
        host.close(*session).await.expect("close session");
    }
}

/// Opens the session's journal for a durable-state assertion and closes it.
async fn stored_mails(
    data_root: std::path::PathBuf,
    workspace: Workspace,
    session: dal_core::SessionId,
) -> Result<Vec<dal_core::Mail>, Box<dyn std::error::Error>> {
    let store = dal_store::Store::new(data_root, workspace, dal_core::Product::Dal);
    let (mut journal, _) = store.open_session(session).await?;
    let mails: Vec<dal_core::Mail> = journal
        .records()
        .iter()
        .filter_map(|record| match record {
            Record::Mail(mail) => Some(mail.clone()),
            _ => None,
        })
        .collect();
    journal.close().await?;
    Ok(mails)
}

/// The typed error a refused append must surface: the agents service failed
/// with the journal's injected sync failure, not a receipt. Production keeps
/// `format!("journal write failed: {error}")` with the full `StoreError`
/// cause, so the message is matched by prefix and cause substring, never by
/// an exact layout or session id; the variant and the service are exact.
fn assert_typed_journal_error(reply: &Reply) {
    let Err(ServiceError::Failed { service, message }) = reply else {
        panic!(
            "a failed mail append must surface the typed journal error, never a \
             receipt or another error: {reply:?}"
        );
    };
    assert_eq!(
        *service,
        Some(Service::Agents),
        "the failure is attributed to the agents service: {reply:?}"
    );
    assert!(
        message.starts_with("journal write failed: ") && message.contains("injected sync failure"),
        "the failure carries the journal's cause: {message}"
    );
}

/// The failure control: one durable self-send on a live, prompted journal
/// still answers with its computed receipt, and the record lands once.
#[tokio::test]
async fn a_durable_self_send_still_reports_its_receipt() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let tool = probe_tool_name("control");
    let (seen, extension) =
        self_send_extension(&tool, Arc::clone(&target), MailMode::NextTurn, None);
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    target_cell(&target, session);

    // The tool call runs inside the root's own turn, so the journal is
    // file-backed before the send: the receipt may only rest on a real
    // append.
    let mut subscription = run_prompt(&root, "send yourself mail").await;
    turn_ended(&mut subscription).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            replies.as_slice(),
            [Ok(delivered(Receipt::Buffered))],
            "a durable NextTurn self-send reports its computed receipt: {replies:?}"
        );
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert_eq!(
        mails.len(),
        1,
        "the accepted self-send is journaled exactly once: {mails:?}"
    );
    assert!(
        mails[0].mode == MailMode::NextTurn && mails[0].text.as_ref() == "probe",
        "the journaled mail keeps its mode and text: {mails:?}"
    );
    Ok(())
}

/// The regression: a send whose mail append fails at sync must surface the
/// typed journal error to the sender, never `Delivered` or `Buffered`, and
/// the message must stay out of the journal.
#[tokio::test]
async fn a_failed_mail_append_is_not_acknowledged_as_delivered_or_buffered() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let (entered, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let tool = probe_tool_name("fault");
    let (seen, extension) = self_send_extension(
        &tool,
        Arc::clone(&target),
        MailMode::NextTurn,
        Some((Arc::clone(&entered), Arc::clone(&gate))),
    );
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    target_cell(&target, session);

    let subscription = run_prompt(&root, "send yourself mail").await;
    // The tool pauses before its send with no batch in flight, so the armed
    // fault can only fire on the send's own mail batch.
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("the tool call to pause before its send");
    let store = dal_store::Store::new(
        live.tmp.path().join("data"),
        live.workspace.clone(),
        dal_core::Product::Dal,
    );
    store.arm_append_sync_fault(session);
    gate.notify_one();
    one_reply(&seen).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reply = replies
            .first()
            .expect("one_reply guarantees a recorded reply");
        assert_typed_journal_error(reply);
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    // The faulted message must stay out of the journal: the sender must not
    // have been told it was stored, and indeed it was not stored.
    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert!(
        mails.iter().all(|mail| mail.text.as_ref() != "probe"),
        "the failed append's message must stay out of the journal: {mails:?}"
    );
    Ok(())
}

/// The Steer ordering defect: `on_mail` steps the fold — queueing live
/// steer input into the running turn — before the append, so a failed
/// append must still surface the typed error instead of `Delivered`, and
/// the fold must never have queued the message: the public stats snapshot
/// exposes the queued-steer count, and the reorder must leave it at zero.
#[tokio::test]
async fn a_failed_steer_append_is_not_acknowledged_as_delivered() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let (entered, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let tool = probe_tool_name("steer-fault");
    let (seen, extension) = self_send_extension(
        &tool,
        Arc::clone(&target),
        MailMode::Steer,
        Some((Arc::clone(&entered), Arc::clone(&gate))),
    );
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    target_cell(&target, session);

    let subscription = run_prompt(&root, "steer yourself mail").await;
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("the tool call to pause before its send");
    let store = dal_store::Store::new(
        live.tmp.path().join("data"),
        live.workspace.clone(),
        dal_core::Product::Dal,
    );
    store.arm_append_sync_fault(session);
    gate.notify_one();
    one_reply(&seen).await;

    // The fold must never have queued the steer. The tool parks with the
    // reply recorded and its call still in flight, so no fold boundary can
    // run: under the current step-before-append order the send queued the
    // text and on_mail's own execute() refreshed the snapshot to 1; the
    // append-before-step repair leaves it at 0.
    let view = root.view(dal_core::PageReq::default())?;
    assert_eq!(
        view.stats.steers_queued, 0,
        "a failed steer append must not leave the message queued in the \
         running turn: stats {:?}",
        view.stats
    );

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reply = replies
            .first()
            .expect("one_reply guarantees a recorded reply");
        assert_typed_journal_error(reply);
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert!(
        mails.iter().all(|mail| mail.text.as_ref() != "probe"),
        "the failed steer append's message must stay out of the journal: {mails:?}"
    );
    Ok(())
}

/// The `Aside` receipt is precomputed as `Delivered` without any journal
/// state, so it is the mode most likely to answer before the append result:
/// a failed append must still surface the typed journal error and keep the
/// message out of the journal.
#[tokio::test]
async fn a_failed_aside_append_is_not_acknowledged_as_delivered() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let (entered, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let tool = probe_tool_name("aside-fault");
    let (seen, extension) = self_send_extension(
        &tool,
        Arc::clone(&target),
        MailMode::Aside,
        Some((Arc::clone(&entered), Arc::clone(&gate))),
    );
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    target_cell(&target, session);

    let subscription = run_prompt(&root, "aside yourself mail").await;
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("the tool call to pause before its send");
    let store = dal_store::Store::new(
        live.tmp.path().join("data"),
        live.workspace.clone(),
        dal_core::Product::Dal,
    );
    store.arm_append_sync_fault(session);
    gate.notify_one();
    one_reply(&seen).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reply = replies
            .first()
            .expect("one_reply guarantees a recorded reply");
        assert_typed_journal_error(reply);
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert!(
        mails.iter().all(|mail| mail.text.as_ref() != "probe"),
        "the failed aside append's message must stay out of the journal: {mails:?}"
    );
    Ok(())
}

/// The aside control on a durable journal: `Aside` computes `Delivered` and
/// must keep that receipt when the append succeeds.
#[tokio::test]
async fn a_durable_aside_send_reports_delivered() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let tool = probe_tool_name("aside");
    let (seen, extension) = self_send_extension(&tool, Arc::clone(&target), MailMode::Aside, None);
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    target_cell(&target, session);

    let mut subscription = run_prompt(&root, "send yourself an aside").await;
    turn_ended(&mut subscription).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            replies.as_slice(),
            [Ok(delivered(Receipt::Delivered))],
            "a durable aside self-send reports its computed receipt: {replies:?}"
        );
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert_eq!(
        mails.len(),
        1,
        "the accepted aside send is journaled exactly once: {mails:?}"
    );
    assert_eq!(mails[0].mode, MailMode::Aside, "the mode survives");
    Ok(())
}

/// The missing-recipient contract: `Gone` is refused before any journal
/// work and keeps its receipt.
#[tokio::test]
async fn a_missing_recipient_keeps_the_gone_contract() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let tool = probe_tool_name("gone");
    let (seen, extension) = self_send_extension(&tool, Arc::clone(&target), MailMode::Aside, None);
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    // The target never opened: `agent_send` refuses with `Gone` before any
    // journal work.
    target_cell(&target, dal_core::SessionId::new_v7());

    let mut subscription = run_prompt(&root, "mail a missing session").await;
    turn_ended(&mut subscription).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            replies.as_slice(),
            [Ok(delivered(Receipt::Gone))],
            "a missing recipient keeps the Gone contract: {replies:?}"
        );
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert!(
        mails.iter().all(|mail| mail.text.as_ref() != "probe"),
        "a refused Gone send never appends: {mails:?}"
    );
    Ok(())
}

/// Live-steer saturation bound: the loop stops at the first real `Full`
/// receipt, and the bound plus the saturated gate hang-guards a future that
/// never fills.
const STEER_SATURATION_BOUND: usize = 128;

struct FullSend {
    name: Name,
    spec: Arc<ToolSpec>,
    target: Target,
    seen: Arc<Mutex<Vec<Reply>>>,
}

impl Tool for FullSend {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let target = Arc::clone(&self.target);
        let seen = Arc::clone(&self.seen);
        Box::pin(async move {
            let to = target_of(&target);
            let services = cx.services();
            let caller = cx.caller();
            for index in 0..=MAILBOX_WAITING_LIMIT {
                let text = if index == MAILBOX_WAITING_LIMIT {
                    "overflow".to_string()
                } else {
                    format!("mail-{index}")
                };
                let reply = services
                    .agents(
                        caller,
                        AgentsOp::Send {
                            to,
                            text: text.into(),
                            mode: MailMode::NextTurn,
                            reply_to: None,
                        },
                    )
                    .await;
                seen.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(reply);
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text("done")))
        })
    }
}

struct SteerSaturate {
    name: Name,
    spec: Arc<ToolSpec>,
    target: Target,
    seen: Arc<Mutex<Vec<Reply>>>,
    entered: Arc<Notify>,
    gate: Arc<Notify>,
    saturated: Arc<Notify>,
}

impl Tool for SteerSaturate {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Other)
    }

    fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let target = Arc::clone(&self.target);
        let seen = Arc::clone(&self.seen);
        let entered = Arc::clone(&self.entered);
        let gate = Arc::clone(&self.gate);
        let saturated = Arc::clone(&self.saturated);
        Box::pin(async move {
            entered.notify_one();
            tokio::select! {
                () = gate.notified() => {}
                () = cx.cancel().cancelled() => return ToolOutcome::Interrupted,
            }
            let to = target_of(&target);
            for index in 0..STEER_SATURATION_BOUND {
                let reply = cx
                    .services()
                    .agents(
                        cx.caller(),
                        AgentsOp::Send {
                            to,
                            text: format!("steer-{index}").into(),
                            mode: MailMode::Steer,
                            reply_to: None,
                        },
                    )
                    .await;
                let full = reply == Ok(delivered(Receipt::Full));
                seen.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(reply);
                if full {
                    break;
                }
            }
            saturated.notify_one();
            // Park with the call still in flight: no fold boundary can
            // drain the queued steers before the test snapshots them.
            cx.cancel().cancelled().await;
            ToolOutcome::Interrupted
        })
    }
}

/// The `Full` contract: the message past the waiting limit reports `Full`
/// and never appends, while the accepted messages report `Buffered` and are
/// journaled.
#[tokio::test]
async fn a_full_mailbox_refuses_without_storing_the_refused_message() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let seen = Arc::new(Mutex::new(Vec::new()));

    let tool = probe_tool_name("full");
    let name = Name::parse(&tool).expect("tool name");
    let extension = ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        ServiceSet::from_names(["agents"]).expect("agents service"),
    )
    .expect("builder")
    .tool(
        Arc::new(FullSend {
            name: name.clone(),
            spec: Arc::new(ToolSpec {
                name,
                description: "mailbox saturation probe".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                grammar: None,
            }),
            target: Arc::clone(&target),
            seen: Arc::clone(&seen),
        }),
        Visibility::Model,
    )
    .build()
    .expect("full extension");
    // The shared scripted queue serves every host session in order: the
    // recipient's warm turn and the root's saturation turn take exactly the
    // three steps below.
    let live = host_with_steps(
        extension,
        format!("{}{}{}", end_turn(), tool_step(&tool), end_turn()),
    )
    .await;
    let root = open_root(&live).await;
    let session = root.session();
    let recipient = open_child(&live, session, "mailbox-full").await;
    let recipient_session = recipient.session();
    // The recipient's journal must be file-backed so its waiting-limit scan
    // reads durable state; the sends themselves then race no fold step.
    warm(&recipient).await;
    target_cell(&target, recipient_session);

    let mut subscription = run_prompt(&root, "saturate the mailbox").await;
    turn_ended(&mut subscription).await;

    {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            replies.len(),
            MAILBOX_WAITING_LIMIT + 1,
            "every send answers with a receipt: {:?}",
            replies.len()
        );
        assert!(
            replies[..MAILBOX_WAITING_LIMIT]
                .iter()
                .all(|reply| *reply == Ok(delivered(Receipt::Buffered))),
            "accepted messages report Buffered: {replies:?}"
        );
        assert_eq!(
            replies[MAILBOX_WAITING_LIMIT],
            Ok(delivered(Receipt::Full)),
            "the message past the limit reports Full: {:?}",
            replies[MAILBOX_WAITING_LIMIT]
        );
    }

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(recipient);
    // The recipient closes first: closing the root reclaims its live
    // descendants, and a second close of a reclaimed id is an error.
    close_all(&host, &[recipient_session, session]).await;
    drop(host);

    // A `Full` receipt never appends: the refused message stays out of the
    // journal while exactly the accepted messages are stored.
    let mails = stored_mails(tmp.path().join("data"), workspace, recipient_session).await?;
    assert_eq!(
        mails.len(),
        MAILBOX_WAITING_LIMIT,
        "exactly the accepted messages are journaled: {:?}",
        mails.len()
    );
    assert!(
        mails
            .iter()
            .all(|mail| mail.mode == MailMode::NextTurn && mail.text.as_ref() != "overflow"),
        "the refused message must stay out of the journal: {:?}",
        mails.last().map(|mail| mail.text.as_ref())
    );
    Ok(())
}

/// The `Steer`-`Full` boundary: filling the live steer queue of the running
/// turn through the public `AgentsOp::Send` surface reports `Full` once the
/// fold's bounded queue refuses, and the refused steer neither queues nor
/// appends while the accepted steers stay journaled.
///
/// The test fills to the real `Full` receipt instead of a copied capacity. The
/// tool parks with its call still in flight, so no settle or boundary can
/// drain the queue before the snapshot.
#[tokio::test]
async fn a_full_steer_queue_refuses_without_queuing_or_storing_the_refused_message() -> TestResult {
    let target: Target = Arc::new(Mutex::new(None));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (entered, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let saturated = Arc::new(Notify::new());

    let tool = probe_tool_name("steer-full");
    let name = Name::parse(&tool).expect("tool name");
    let extension = ExtensionBuilder::new(
        "fixture",
        "0.1.0",
        ServiceSet::from_names(["agents"]).expect("agents service"),
    )
    .expect("builder")
    .tool(
        Arc::new(SteerSaturate {
            name: name.clone(),
            spec: Arc::new(ToolSpec {
                name,
                description: "steer saturation probe".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
                grammar: None,
            }),
            target: Arc::clone(&target),
            seen: Arc::clone(&seen),
            entered: Arc::clone(&entered),
            gate: Arc::clone(&gate),
            saturated: Arc::clone(&saturated),
        }),
        Visibility::Model,
    )
    .build()
    .expect("steer-full extension");
    let live = host_with_steps(extension, format!("{}{}", tool_step(&tool), end_turn())).await;
    let root = open_root(&live).await;
    let session = root.session();
    // The tool call runs inside the root's own turn, so each self-send
    // steers the live queue of that same running turn.
    target_cell(&target, session);

    let subscription = run_prompt(&root, "saturate the steer queue").await;
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("the tool call to pause before its sends");
    gate.notify_one();
    tokio::time::timeout(WAIT, saturated.notified())
        .await
        .expect("the steer queue to report Full while the turn stays in flight");

    let full_at = {
        let replies = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let full_at = replies
            .iter()
            .position(|reply| *reply == Ok(delivered(Receipt::Full)))
            .expect("the live steer queue to refuse with Full");
        assert!(
            full_at > 0,
            "the queue must accept at least one steer before Full: {replies:?}"
        );
        assert!(
            replies[..full_at]
                .iter()
                .all(|reply| *reply == Ok(delivered(Receipt::Delivered))),
            "accepted steers report Delivered: {replies:?}"
        );
        full_at
    };

    // The refused steer must not queue: the snapshot runs while the tool
    // still parks its call, so the count reads the sends' own effect.
    let view = root.view(dal_core::PageReq::default())?;
    assert_eq!(
        usize::try_from(view.stats.steers_queued).expect("steer count fits in usize"),
        full_at,
        "Full must not queue the refused steer: stats {:?}",
        view.stats
    );

    let Live {
        host,
        workspace,
        tmp,
    } = live;
    drop(subscription);
    drop(root);
    close_all(&host, &[session]).await;
    drop(host);

    // A `Full` receipt never appends: the refused steer stays out of the
    // journal while exactly the accepted steers are stored.
    let mails = stored_mails(tmp.path().join("data"), workspace, session).await?;
    assert_eq!(
        mails.len(),
        full_at,
        "exactly the accepted steers are journaled: {:?}",
        mails.len()
    );
    for (index, mail) in mails.iter().enumerate() {
        assert_eq!(mail.mode, MailMode::Steer, "accepted steer mode");
        assert_eq!(
            mail.text.as_ref(),
            format!("steer-{index}"),
            "only the accepted steers are retained in order"
        );
    }
    Ok(())
}
