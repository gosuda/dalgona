#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]
#![expect(missing_docs, reason = "SC test")]

mod support;

use std::{
    collections::BTreeMap,
    error::Error,
    future::pending,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use dal_agent::{
    Env, ServiceError, SessionRef,
    ext::{
        BoxFuture, CommandCx, CommandHandler, ExtensionBuilder, Hook, HookCx, HookError, Services,
    },
};
use dal_core::ext::Mail;
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, CallId, ClientId, Command, CommandName, CommandSpec, Config,
    ConfigProduct, MailMode, Output, PageReq, Receipt, Reply, RequestParams, Service, ServiceSet,
    Workspace,
};
use support::{TestDir, scripted_session};
use tokio::sync::watch;

const SCRIPTED_RESPONSE: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"member finished"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

struct MailboxState {
    phase: AtomicUsize,
    member_read_started: AtomicBool,
    hook_entries: AtomicUsize,
    hook_parented: AtomicUsize,
    last_parent: Mutex<Option<dal_core::SessionId>>,
    member_ready: watch::Sender<bool>,
    first_read_enabled: watch::Sender<bool>,
    first_read_done: watch::Sender<bool>,
    second_read_enabled: watch::Sender<bool>,
    second_read_done: watch::Sender<bool>,
    capacity_ready: watch::Sender<bool>,
    sessions: Mutex<Option<(dal_core::SessionId, dal_core::SessionId)>>,
    external: Mutex<Option<dal_core::SessionId>>,
    seed_cursor: Mutex<Option<dal_core::EntryId>>,
    reads: Mutex<Option<MailboxReads>>,
    receipts: Mutex<Option<MailboxReceipts>>,
}

#[derive(Clone)]
struct MailboxReads {
    first: Vec<Mail>,
    first_next: Option<dal_core::EntryId>,
    repeated_first: Vec<Mail>,
    repeated_first_next: Option<dal_core::EntryId>,
    repeated_second: Vec<Mail>,
    repeated_second_next: Option<dal_core::EntryId>,
}

#[derive(Clone)]
struct MailboxReceipts {
    initial: Vec<Receipt>,
    third: Receipt,
    waiting: Vec<Receipt>,
    full: Receipt,
    external: Receipt,
    finished: Receipt,
}

impl MailboxState {
    fn new() -> Self {
        Self {
            phase: AtomicUsize::new(0),
            member_read_started: AtomicBool::new(false),
            hook_entries: AtomicUsize::new(0),
            hook_parented: AtomicUsize::new(0),
            last_parent: Mutex::new(None),
            member_ready: watch::channel(false).0,
            first_read_enabled: watch::channel(false).0,
            first_read_done: watch::channel(false).0,
            second_read_enabled: watch::channel(false).0,
            second_read_done: watch::channel(false).0,
            capacity_ready: watch::channel(false).0,
            sessions: Mutex::new(None),
            external: Mutex::new(None),
            seed_cursor: Mutex::new(None),
            reads: Mutex::new(None),
            receipts: Mutex::new(None),
        }
    }
}

struct MailboxCommand {
    state: Arc<MailboxState>,
}

impl CommandHandler for MailboxCommand {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        let state = Arc::clone(&self.state);
        let caller = cx.caller().clone();
        let services = Arc::clone(cx.services());
        let parent = cx.session();
        Box::pin(async move {
            let external = state
                .external
                .lock()
                .expect("external session mutex")
                .as_ref()
                .copied()
                .ok_or_else(|| service_failure("external session is not registered"))?;
            let receipts = run_mailbox_probe(&state, &services, &caller, parent, external).await?;
            *state.receipts.lock().expect("receipt mutex") = Some(receipts);
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

struct MailboxHook {
    state: Arc<MailboxState>,
}

impl Hook<dal_core::ext::BeforeRequest, Option<RequestParams>> for MailboxHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeRequest,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        let state = Arc::clone(&self.state);
        state.hook_entries.fetch_add(1, Ordering::SeqCst);
        let Some(parent) = cx.parent else {
            return Box::pin(async { Ok(None) });
        };
        state.hook_parented.fetch_add(1, Ordering::SeqCst);
        *state.last_parent.lock().expect("hook parent mutex") = Some(parent);
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        let phase = state.phase.load(Ordering::SeqCst);
        Box::pin(async move {
            match phase {
                1 => {
                    if !state.member_read_started.swap(true, Ordering::SeqCst) {
                        read_member_mail(&state, &services, &caller).await?;
                    }
                }
                2 => {
                    state.capacity_ready.send_replace(true);
                    pending::<()>().await;
                }
                _ => return Err(hook_failure("mailbox hook entered an unknown phase")),
            }
            Ok(None)
        })
    }
}

async fn run_mailbox_probe(
    state: &MailboxState,
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    parent: dal_core::SessionId,
    external_session: dal_core::SessionId,
) -> Result<MailboxReceipts, ServiceError> {
    state.phase.store(1, Ordering::SeqCst);
    let member = start_member(services, caller, "mailbox-member", "member mailbox").await?;
    *state.sessions.lock().expect("session mutex") = Some((parent, member));
    let mut member_ready = state.member_ready.subscribe();
    if let Err(error) = wait_for_flag(
        &mut member_ready,
        "mailbox member hook did not reach its rendezvous",
    )
    .await
    {
        return Err(service_failure(&format!(
            "{error}; hook_entries={} hook_parented={} last_parent={:?} phase={}",
            state.hook_entries.load(Ordering::SeqCst),
            state.hook_parented.load(Ordering::SeqCst),
            *state.last_parent.lock().expect("hook parent mutex"),
            state.phase.load(Ordering::SeqCst),
        )));
    }

    let seed = send(
        services,
        caller,
        member,
        "cursor-seed",
        MailMode::Aside,
        None,
    )
    .await?;
    state.first_read_enabled.send_replace(true);
    let mut first_read_done = state.first_read_done.subscribe();
    wait_for_flag(
        &mut first_read_done,
        "mailbox first read did not reach its rendezvous",
    )
    .await?;
    let reply_to = state
        .seed_cursor
        .lock()
        .expect("seed cursor mutex")
        .as_ref()
        .copied()
        .ok_or_else(|| service_failure("seed mailbox cursor is missing"))?;
    let aside = send(
        services,
        caller,
        member,
        "aside-one",
        MailMode::Aside,
        Some(reply_to),
    )
    .await?;
    let steer = send(
        services,
        caller,
        member,
        "steer-two",
        MailMode::Steer,
        Some(reply_to),
    )
    .await?;
    let next_turn = send(
        services,
        caller,
        member,
        "next-turn-three",
        MailMode::NextTurn,
        Some(reply_to),
    )
    .await?;
    state.second_read_enabled.send_replace(true);
    let mut second_read_done = state.second_read_done.subscribe();
    wait_for_flag(
        &mut second_read_done,
        "mailbox cursor reads did not reach their rendezvous",
    )
    .await?;
    let first_child = wait_for_child(services, caller, member).await?;
    if !matches!(first_child, AgentsReply::Await { .. }) {
        return Err(service_failure("mailbox member did not finish"));
    }
    let finished = send(
        services,
        caller,
        member,
        "after-finish",
        MailMode::Aside,
        None,
    )
    .await?;

    state.phase.store(2, Ordering::SeqCst);
    let capacity_member = start_member(
        services,
        caller,
        "mailbox-capacity",
        "hold mailbox capacity",
    )
    .await?;
    let mut capacity_ready = state.capacity_ready.subscribe();
    wait_for_flag(
        &mut capacity_ready,
        "mailbox capacity hook did not reach its rendezvous",
    )
    .await?;
    let messages = (0..100)
        .map(|index| format!("queued-{index}"))
        .collect::<Vec<_>>();
    let waiting = futures::future::join_all(messages.iter().map(|text| {
        send(
            services,
            caller,
            capacity_member,
            text,
            MailMode::NextTurn,
            None,
        )
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let full = send(
        services,
        caller,
        capacity_member,
        "queue-overflow",
        MailMode::NextTurn,
        None,
    )
    .await?;
    let external = send(
        services,
        caller,
        external_session,
        "not-in-tree",
        MailMode::Aside,
        None,
    )
    .await?;
    match services
        .agents(
            caller,
            AgentsOp::Cancel {
                id: capacity_member,
            },
        )
        .await?
    {
        AgentsReply::Cancelled { id } if id == capacity_member => {}
        _ => return Err(service_failure("capacity member did not cancel")),
    }
    let capacity_result = wait_for_child(services, caller, capacity_member).await?;
    if !matches!(
        capacity_result,
        AgentsReply::Cancelled { .. } | AgentsReply::Await { .. }
    ) {
        return Err(service_failure("capacity member did not end"));
    }
    Ok(MailboxReceipts {
        initial: vec![seed, aside, steer],
        third: next_turn,
        waiting,
        full,
        external,
        finished,
    })
}

async fn read_member_mail(
    state: &MailboxState,
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
) -> Result<(), HookError> {
    state.member_ready.send_replace(true);
    let mut first_read_enabled = state.first_read_enabled.subscribe();
    wait_for_hook_flag(&mut first_read_enabled).await?;
    let (first, first_next) = receive(services, caller, None).await?;
    let cursor = first_next.ok_or_else(|| hook_failure("first mailbox read has no cursor"))?;
    *state.seed_cursor.lock().expect("seed cursor mutex") = Some(cursor);
    state.first_read_done.send_replace(true);
    let mut second_read_enabled = state.second_read_enabled.subscribe();
    wait_for_hook_flag(&mut second_read_enabled).await?;
    let (repeated_first, repeated_first_next) = receive(services, caller, Some(cursor)).await?;
    let (repeated_second, repeated_second_next) = receive(services, caller, Some(cursor)).await?;
    *state.reads.lock().expect("mailbox read mutex") = Some(MailboxReads {
        first,
        first_next,
        repeated_first,
        repeated_first_next,
        repeated_second,
        repeated_second_next,
    });
    state.second_read_done.send_replace(true);
    Ok(())
}

async fn start_member(
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    name: &str,
    prompt: &str,
) -> Result<dal_core::SessionId, ServiceError> {
    match services
        .agents(
            caller,
            AgentsOp::Start(AgentStart {
                call: CallId::new(name),
                name: name.into(),
                prompt: prompt.into(),
                model: None,
                role: None,
                system: None,
                tools: None,
                workspace: None,
            }),
        )
        .await?
    {
        AgentsReply::Started { id } => Ok(id),
        _ => Err(service_failure("member session did not start")),
    }
}

async fn send(
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    to: dal_core::SessionId,
    text: &str,
    mode: MailMode,
    reply_to: Option<dal_core::EntryId>,
) -> Result<Receipt, ServiceError> {
    match services
        .agents(
            caller,
            AgentsOp::Send {
                to,
                text: text.into(),
                mode,
                reply_to,
            },
        )
        .await?
    {
        AgentsReply::Delivered(receipt) => Ok(receipt),
        _ => Err(service_failure("mail send returned no receipt")),
    }
}

async fn receive(
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    after: Option<dal_core::EntryId>,
) -> Result<(Vec<Mail>, Option<dal_core::EntryId>), HookError> {
    match services
        .agents(
            caller,
            AgentsOp::Recv {
                after,
                timeout: Some(Duration::from_secs(5)),
            },
        )
        .await
    {
        Ok(AgentsReply::Received { mail, next }) => Ok((mail, next)),
        Ok(_) => Err(hook_failure("mail receive returned no cursor")),
        Err(error) => Err(hook_failure(error.to_string())),
    }
}

async fn wait_for_child(
    services: &Arc<dyn Services>,
    caller: &dal_agent::ext::Caller,
    id: dal_core::SessionId,
) -> Result<AgentsReply, ServiceError> {
    loop {
        let reply = services
            .agents(
                caller,
                AgentsOp::Await {
                    id,
                    timeout: Some(Duration::from_secs(10)),
                },
            )
            .await?;
        if matches!(reply, AgentsReply::Pending { .. }) {
            continue;
        }
        return Ok(reply);
    }
}

async fn wait_for_flag(
    flag: &mut watch::Receiver<bool>,
    timeout_message: &'static str,
) -> Result<(), ServiceError> {
    if *flag.borrow_and_update() {
        return Ok(());
    }
    tokio::time::timeout(Duration::from_secs(30), flag.changed())
        .await
        .map_err(|_| service_failure(timeout_message))?
        .map_err(|_| service_failure("mailbox hook rendezvous closed"))
}

async fn wait_for_hook_flag(flag: &mut watch::Receiver<bool>) -> Result<(), HookError> {
    if *flag.borrow_and_update() {
        return Ok(());
    }
    tokio::time::timeout(Duration::from_secs(30), flag.changed())
        .await
        .map_err(|_| hook_failure("parent did not release mailbox read"))?
        .map_err(|_| hook_failure("mailbox read rendezvous closed"))
}

fn service_failure(message: &str) -> ServiceError {
    ServiceError::failed(Some(Service::Agents), message)
}

fn hook_failure(message: impl Into<Box<str>>) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

const fn expected_receipt(mode: MailMode) -> Receipt {
    match mode {
        MailMode::Aside => Receipt::Delivered,
        MailMode::Steer => Receipt::Delivered,
        MailMode::NextTurn => Receipt::Buffered,
        _ => Receipt::Buffered,
    }
}

#[tokio::test]
async fn mailbox_is_fifo_cursor_read_and_reports_full_or_gone()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let fixture = data.path().join("mailbox-scripted.jsonl");
    std::fs::write(
        &fixture,
        format!("{SCRIPTED_RESPONSE}\n{SCRIPTED_RESPONSE}\n"),
    )?;
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
    let state = Arc::new(MailboxState::new());
    let extension =
        ExtensionBuilder::new("mailbox-gate", "0.1.0", ServiceSet::from_names(["agents"])?)?
            .command(
                CommandSpec {
                    name: CommandName::parse("mailbox-probe")?,
                    summary: "Exercise the member mailbox.".into(),
                    args_hint: None,
                },
                Arc::new(MailboxCommand {
                    state: Arc::clone(&state),
                }),
            )
            .on_before_request(MailboxHook {
                state: Arc::clone(&state),
            })
            .build()?;
    let mut product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    product.extensions.push(extension);
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let session = SessionRef::New {
        workspace: Workspace::new(workspace.path().to_path_buf())?,
        name: Some("mailbox-gate".into()),
    };
    let harness = scripted_session(product, config, env, session).await?;
    let external_workspace = TestDir::new()?;
    let external = harness
        .host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(external_workspace.path().to_path_buf())?,
            },
            ClientId::new("external"),
        )
        .await?;
    let external_id = external.view(PageReq::default())?.session.id;
    *state.external.lock().expect("external session mutex") = Some(external_id);
    let reply = tokio::time::timeout(
        Duration::from_secs(120),
        harness.agent.submit(Command::Run {
            name: "mailbox-probe".into(),
            args: "{}".into(),
            expected: None,
        }),
    )
    .await??;
    assert!(matches!(reply, Reply::Done(_)));
    let (parent, member) = state.sessions.lock().expect("session mutex").unwrap();
    let reads = state
        .reads
        .lock()
        .expect("mailbox read mutex")
        .clone()
        .ok_or("mailbox reads were not recorded")?;
    let receipts = state
        .receipts
        .lock()
        .expect("receipt mutex")
        .clone()
        .ok_or("mailbox receipts were not recorded")?;

    assert_eq!(
        receipts.initial,
        [
            expected_receipt(MailMode::Aside),
            expected_receipt(MailMode::Aside),
            expected_receipt(MailMode::Steer),
        ]
    );
    assert_eq!(receipts.third, expected_receipt(MailMode::NextTurn));
    assert_eq!(receipts.waiting.len(), 100);
    assert!(
        receipts
            .waiting
            .iter()
            .all(|receipt| *receipt == Receipt::Buffered)
    );
    assert_eq!(receipts.full, Receipt::Full);
    assert_eq!(receipts.external, Receipt::Gone);
    assert_eq!(receipts.finished, Receipt::Gone);
    assert_eq!(reads.first.len(), 1);
    assert_eq!(reads.first[0].text.as_ref(), "cursor-seed");
    assert!(reads.first_next.is_some());
    assert!(reads.repeated_first_next.is_some());
    assert_ne!(reads.first_next, reads.repeated_first_next);
    assert!(
        reads
            .first
            .iter()
            .all(|mail| mail.from == parent && mail.to == member && mail.reply_to.is_none())
    );
    assert_eq!(reads.first[0].mode, MailMode::Aside);
    assert_eq!(reads.repeated_first, reads.repeated_second);
    assert_eq!(reads.repeated_first_next, reads.repeated_second_next);
    assert_eq!(reads.repeated_first.len(), 3);
    assert_eq!(
        reads
            .repeated_first
            .iter()
            .map(|mail| mail.text.as_ref())
            .collect::<Vec<_>>(),
        ["aside-one", "steer-two", "next-turn-three"]
    );
    assert!(reads.repeated_first.iter().all(|mail| {
        mail.from == parent && mail.to == member && mail.reply_to == reads.first_next
    }));
    assert_eq!(reads.repeated_first[0].mode, MailMode::Aside);
    assert_eq!(reads.repeated_first[1].mode, MailMode::Steer);
    assert_eq!(reads.repeated_first[2].mode, MailMode::NextTurn);
    assert!(reads.repeated_first_next.is_some());

    let _ = harness.host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
