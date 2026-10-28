// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: the goal and continuation commands
//! answer through the real host with their exact refusal texts, so a
//! wording change fails here instead of drifting on users.
//!
//! The mailbox criterion drives the real Dalgona parent, a real child
//! session, and an out-of-tree session through the `agents` service:
//! per-pair FIFO, exact `Mail` fields, the journal record preceding the
//! visible receipt, cursor reads that never consume, the named `Full`
//! refusal on the 101st waiting `NextTurn`, delivery-then-accept while
//! the delivered records stay retained (expected red until the fold
//! gains a typed mail-delivery record), and the named `Gone` refusals
//! for out-of-tree and finished recipients before any record write.
use dalgona_gates::support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::grants::{GrantKey, GrantStore};
use dal_agent::ext::{
    BoxFuture, Caller, CommandCx, CommandHandler, ExtensionBuilder, Hook, HookCx, HookError,
    Services,
};
use dal_agent::{Env, Host, ServiceError, SessionRef};
use dal_core::ext::Mail;
use dal_core::{
    AgentStart, AgentsOp, AgentsReply, CallId, ClientId, EntryId, MailMode, Output, PageReq,
    Receipt, Reply, RequestParams, Service, ServiceSet, SessionId, Timestamp, Workspace,
};
use sonic_rs::JsonValueTrait as _;
use tokio::sync::watch;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn goal_commands_answer_with_exact_texts() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-goal-cmd-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, support::text_step("ready"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let product = support::build_product(data.clone(), Some(&user))?;
        for extension in &product.extensions {
            let services = extension.inject().capabilities();
            if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
                continue;
            }
            dal_agent::ext::grants::GrantStore::new(data.clone())
                .grant(
                    dal_agent::ext::grants::GrantKey::for_extension(extension, &workspace)
                        .expect("registered extension has a code identity"),
                    dal_core::ClientId::new("goal-cmd-gate"),
                    dal_core::Timestamp::now(),
                )
                .await?;
        }
        drop(product);
        let host = support::start_dalgona_with_config(data.clone(), Some(&user)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace)?,
                    name: None,
                },
                dal_core::ClientId::new("goal-cmd-gate"),
            )
            .await?;

        // Empty show on a fresh session.
        assert_eq!(run_command(&agent, "goal", "").await?, "No goal.");
        // Clearing an absent goal keeps the same answer.
        assert_eq!(run_command(&agent, "goal", "clear").await?, "No goal.");

        // Creation, then a second create replaces the goal: the command
        // door creates or replaces, while only the model tool refuses.
        assert!(
            run_command(&agent, "goal", "fix the flaky test")
                .await?
                .contains("goal g1: active")
        );
        let replaced = run_command(&agent, "goal", "another objective").await?;
        assert!(
            replaced.contains("goal g2: active"),
            "second create did not replace the goal: {replaced}"
        );

        // Pause and resume through both doors, exact texts.
        assert_eq!(
            run_command(&agent, "goal", "pause").await?,
            "goal g2 paused."
        );
        assert!(
            run_command(&agent, "goal", "resume")
                .await?
                .starts_with("goal g2 is active."),
            "resume reply drifted"
        );
        assert_eq!(
            run_command(&agent, "continuation", "pause").await?,
            "automatic turns: paused (paused by the user)"
        );
        assert_eq!(
            run_command(&agent, "continuation", "run").await?,
            "automatic turns: run"
        );

        // Unknown command text fails with the typed error, not a panic.
        let unknown = agent
            .submit(dal_core::Command::Run {
                name: "no_such_command".into(),
                args: String::new().into(),
                expected: None,
            })
            .await;
        assert!(unknown.is_err(), "unknown commands must fail");

        let report = host.shutdown(Duration::from_secs(5)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

async fn run_command(
    agent: &dal_agent::Agent,
    name: &str,
    args: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let reply = agent
        .submit(dal_core::Command::Run {
            name: name.into(),
            args: args.into(),
            expected: None,
        })
        .await?;
    let dal_core::Reply::Done(output) = reply else {
        return Err(format!("unexpected command reply: {reply:?}").into());
    };
    let dal_core::Output::Text(text) = output else {
        return Err(format!("unexpected command output: {output:?}").into());
    };
    Ok(text.to_string())
}

// ---------------------------------------------------------------------------
// Mailbox gate: cursor reads, named `Full`, named `Gone`.
// ---------------------------------------------------------------------------

/// How many `NextTurn` messages the capacity leg queues before the named
/// refusal.
const MAILBOX_QUEUE_COUNT: usize = 100;

/// Rendezvous wait ceiling; every probe wait is bounded by this.
const MAILBOX_FLAG_WAIT: Duration = Duration::from_secs(30);

/// Whole-probe ceiling around the parent command.
const MAILBOX_PROBE_WAIT: Duration = Duration::from_secs(240);

/// Captured mailbox reads from the member session's own cursor walk.
#[derive(Clone)]
struct MailboxReads {
    first: Vec<Mail>,
    first_next: Option<EntryId>,
    repeated_first: Vec<Mail>,
    repeated_first_next: Option<EntryId>,
    repeated_second: Vec<Mail>,
    repeated_second_next: Option<EntryId>,
}

/// Captured send receipts from the parent command, in send order.
#[derive(Clone)]
struct MailboxReceipts {
    /// Seed, aside, and steer receipts of the FIFO leg.
    initial: Vec<Receipt>,
    /// The `NextTurn` receipt closing the FIFO leg.
    third: Receipt,
    /// One receipt per queued capacity message.
    waiting: Vec<Receipt>,
    /// The receipt of the 101st capacity message.
    full: Receipt,
    /// The receipt of the out-of-tree send.
    external: Receipt,
    /// The receipt of the send after the member finished.
    finished: Receipt,
    /// The receipt of the send made at the delivered-queue boundary.
    drain: Receipt,
}

/// Rendezvous flags and captured results shared by the probe command and
/// the request hook.
struct MailboxProbe {
    phase: AtomicUsize,
    member_read_started: AtomicBool,
    member_ready: watch::Sender<bool>,
    first_read_enabled: watch::Sender<bool>,
    first_read_done: watch::Sender<bool>,
    second_read_enabled: watch::Sender<bool>,
    second_read_done: watch::Sender<bool>,
    capacity_ready: watch::Sender<bool>,
    capacity_release: watch::Sender<bool>,
    drain_turn_running: watch::Sender<bool>,
    drain_checked: watch::Sender<bool>,
    external: Mutex<Option<SessionId>>,
    sessions: Mutex<Option<(SessionId, SessionId)>>,
    capacity: Mutex<Option<SessionId>>,
    seed_cursor: Mutex<Option<EntryId>>,
    reads: Mutex<Option<MailboxReads>>,
    receipts: Mutex<Option<MailboxReceipts>>,
}

impl MailboxProbe {
    fn new() -> Self {
        Self {
            phase: AtomicUsize::new(0),
            member_read_started: AtomicBool::new(false),
            member_ready: watch::channel(false).0,
            first_read_enabled: watch::channel(false).0,
            first_read_done: watch::channel(false).0,
            second_read_enabled: watch::channel(false).0,
            second_read_done: watch::channel(false).0,
            capacity_ready: watch::channel(false).0,
            capacity_release: watch::channel(false).0,
            drain_turn_running: watch::channel(false).0,
            drain_checked: watch::channel(false).0,
            external: Mutex::new(None),
            sessions: Mutex::new(None),
            capacity: Mutex::new(None),
            seed_cursor: Mutex::new(None),
            reads: Mutex::new(None),
            receipts: Mutex::new(None),
        }
    }
}

/// Locks a probe mutex without relying on `unwrap`.
fn shared_lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The parent-side command door that drives the whole mailbox walkthrough.
struct ProbeCommand {
    state: Arc<MailboxProbe>,
}

impl CommandHandler for ProbeCommand {
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
            let external = shared_lock(&state.external)
                .as_ref()
                .copied()
                .ok_or_else(|| service_failure("the out-of-tree session is not registered"))?;
            let receipts = run_mailbox_probe(&state, &services, &caller, parent, external).await?;
            *shared_lock(&state.receipts) = Some(receipts);
            Ok(Reply::Done(Output::Nothing))
        })
    }
}

/// The request hook that lets each child session read its own mailbox or
/// hold its turn at a rendezvous point.
struct ProbeHook {
    state: Arc<MailboxProbe>,
}

impl Hook<dal_core::ext::BeforeRequest, Option<RequestParams>> for ProbeHook {
    fn call(
        &self,
        _input: dal_core::ext::BeforeRequest,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<RequestParams>, HookError>> {
        let state = Arc::clone(&self.state);
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        let phase = state.phase.load(Ordering::SeqCst);
        Box::pin(async move {
            match phase {
                0 => Ok(None),
                1 => {
                    if !state.member_read_started.swap(true, Ordering::SeqCst) {
                        read_member_mail(&state, &services, &caller).await?;
                    }
                    Ok(None)
                }
                2 => {
                    state.capacity_ready.send_replace(true);
                    wait_hook_flag(
                        &mut state.capacity_release.subscribe(),
                        "the capacity hold was never released",
                    )
                    .await?;
                    Ok(None)
                }
                3 => {
                    state.drain_turn_running.send_replace(true);
                    wait_hook_flag(
                        &mut state.drain_checked.subscribe(),
                        "the drain rendezvous was never released",
                    )
                    .await?;
                    Ok(None)
                }
                _ => Err(hook_failure("the mailbox hook entered an unknown phase")),
            }
        })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the mailbox probe is one scripted walkthrough of the criterion"
)]
async fn run_mailbox_probe(
    state: &MailboxProbe,
    services: &Arc<dyn Services>,
    caller: &Caller,
    parent: SessionId,
    external: SessionId,
) -> Result<MailboxReceipts, ServiceError> {
    // Leg one: FIFO plus cursor reads against a real child session. The
    // child holds its first turn at the hook while the parent sends, so
    // the receipts are observed before the child's turn resumes; the
    // member's reads then prove every record was already journaled when
    // its send receipt became visible.
    state.phase.store(1, Ordering::SeqCst);
    let member = probe_start(services, caller, "mailbox-member").await?;
    *shared_lock(&state.sessions) = Some((parent, member));
    wait_service_flag(
        &mut state.member_ready.subscribe(),
        "the member session never reached its mailbox hook",
    )
    .await?;
    let seed = probe_send(
        services,
        caller,
        member,
        "cursor-seed",
        MailMode::Aside,
        None,
    )
    .await?;
    state.first_read_enabled.send_replace(true);
    wait_service_flag(
        &mut state.first_read_done.subscribe(),
        "the member never recorded its first mailbox read",
    )
    .await?;
    let reply_to = shared_lock(&state.seed_cursor)
        .as_ref()
        .copied()
        .ok_or_else(|| service_failure("the member never recorded its seed cursor"))?;
    let aside = probe_send(
        services,
        caller,
        member,
        "aside-one",
        MailMode::Aside,
        Some(reply_to),
    )
    .await?;
    let steer = probe_send(
        services,
        caller,
        member,
        "steer-two",
        MailMode::Steer,
        Some(reply_to),
    )
    .await?;
    let next_turn = probe_send(
        services,
        caller,
        member,
        "next-turn-three",
        MailMode::NextTurn,
        Some(reply_to),
    )
    .await?;
    state.second_read_enabled.send_replace(true);
    wait_service_flag(
        &mut state.second_read_done.subscribe(),
        "the member never recorded its repeated cursor reads",
    )
    .await?;
    // Await marks the child reported; the send that follows must then be
    // named `Gone` before any record write.
    probe_await_settled(services, caller, member, "the member never finished").await?;
    let finished = probe_send(
        services,
        caller,
        member,
        "after-finish",
        MailMode::Aside,
        None,
    )
    .await?;

    // Leg two: the named refusal. While the capacity child's first turn
    // is held at its hook, 100 `NextTurn` messages are genuinely waiting
    // and the 101st must be refused as `Full`. The refusal alone does not
    // prove a waiting queue; the drain send below does.
    state.phase.store(2, Ordering::SeqCst);
    let capacity = probe_start(services, caller, "mailbox-capacity").await?;
    *shared_lock(&state.capacity) = Some(capacity);
    wait_service_flag(
        &mut state.capacity_ready.subscribe(),
        "the capacity session never reached its mailbox hook",
    )
    .await?;
    let mut waiting = Vec::with_capacity(MAILBOX_QUEUE_COUNT);
    for index in 0..MAILBOX_QUEUE_COUNT {
        waiting.push(
            probe_send(
                services,
                caller,
                capacity,
                &format!("queued-{index}"),
                MailMode::NextTurn,
                None,
            )
            .await?,
        );
    }
    let full = probe_send(
        services,
        caller,
        capacity,
        "queue-overflow",
        MailMode::NextTurn,
        None,
    )
    .await?;
    let out_of_tree = probe_send(
        services,
        caller,
        external,
        "not-in-tree",
        MailMode::Aside,
        None,
    )
    .await?;

    // Leg three: delivery-then-accept. Release the held turn, let it end,
    // then start the child's next turn and hold it at its hook again: the
    // waiting queue's delivery boundary has now passed, so a new message
    // must be buffered while the delivered records stay retained.
    state.capacity_release.send_replace(true);
    state.phase.store(3, Ordering::SeqCst);
    let mut prompted = false;
    for _ in 0..300 {
        match services
            .agents(
                caller,
                AgentsOp::Prompt {
                    id: capacity,
                    text: "drain turn".into(),
                    interrupt: None,
                    max_steps: None,
                },
            )
            .await?
        {
            AgentsReply::Prompted { .. } => {
                prompted = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    if !prompted {
        return Err(service_failure(
            "the capacity child never accepted its drain turn",
        ));
    }
    wait_service_flag(
        &mut state.drain_turn_running.subscribe(),
        "the drain turn never reached its mailbox hook",
    )
    .await?;
    let drain = probe_send(
        services,
        caller,
        capacity,
        "after-drain",
        MailMode::NextTurn,
        None,
    )
    .await?;
    state.drain_checked.send_replace(true);
    probe_await_settled(
        services,
        caller,
        capacity,
        "the capacity child never finished its drain turn",
    )
    .await?;

    Ok(MailboxReceipts {
        initial: vec![seed, aside, steer],
        third: next_turn,
        waiting,
        full,
        external: out_of_tree,
        finished,
        drain,
    })
}

/// The member-side leg: one cursor walk plus two reads from the same
/// cursor, recorded for the test body.
async fn read_member_mail(
    state: &MailboxProbe,
    services: &Arc<dyn Services>,
    caller: &Caller,
) -> Result<(), HookError> {
    state.member_ready.send_replace(true);
    wait_hook_flag(
        &mut state.first_read_enabled.subscribe(),
        "the parent never released the first mailbox read",
    )
    .await?;
    let (first, first_next) = probe_receive(services, caller, None).await?;
    let cursor =
        first_next.ok_or_else(|| hook_failure("the first mailbox read returned no cursor"))?;
    *shared_lock(&state.seed_cursor) = Some(cursor);
    state.first_read_done.send_replace(true);
    wait_hook_flag(
        &mut state.second_read_enabled.subscribe(),
        "the parent never released the repeated mailbox reads",
    )
    .await?;
    let (repeated_first, repeated_first_next) =
        probe_receive(services, caller, Some(cursor)).await?;
    let (repeated_second, repeated_second_next) =
        probe_receive(services, caller, Some(cursor)).await?;
    *shared_lock(&state.reads) = Some(MailboxReads {
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

/// Starts one probe child session.
async fn probe_start(
    services: &Arc<dyn Services>,
    caller: &Caller,
    name: &str,
) -> Result<SessionId, ServiceError> {
    match services
        .agents(
            caller,
            AgentsOp::Start(AgentStart {
                call: CallId::new(name),
                name: Some(name.into()),
                prompt: "run one quiet turn".into(),
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
        _ => Err(service_failure("the probe child session did not start")),
    }
}

/// Sends one mailbox message and returns its receipt.
async fn probe_send(
    services: &Arc<dyn Services>,
    caller: &Caller,
    to: SessionId,
    text: &str,
    mode: MailMode,
    reply_to: Option<EntryId>,
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
        _ => Err(service_failure("the mail send returned no receipt")),
    }
}

/// Reads the caller's own mailbox after a cursor.
async fn probe_receive(
    services: &Arc<dyn Services>,
    caller: &Caller,
    after: Option<EntryId>,
) -> Result<(Vec<Mail>, Option<EntryId>), HookError> {
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
        Ok(_) => Err(hook_failure("the mailbox read returned no messages")),
        Err(error) => Err(hook_failure(error.to_string())),
    }
}

/// Waits until the child finished; the await is what marks a child
/// reported, so the finished-`Gone` send depends on it.
async fn probe_await_settled(
    services: &Arc<dyn Services>,
    caller: &Caller,
    id: SessionId,
    message: &'static str,
) -> Result<(), ServiceError> {
    for _ in 0..30 {
        match services
            .agents(
                caller,
                AgentsOp::Await {
                    id,
                    timeout: Some(Duration::from_secs(10)),
                },
            )
            .await?
        {
            AgentsReply::Pending { .. } => {}
            AgentsReply::Await { .. } => return Ok(()),
            _ => return Err(service_failure(message)),
        }
    }
    Err(service_failure(message))
}

async fn wait_service_flag(
    flag: &mut watch::Receiver<bool>,
    message: &'static str,
) -> Result<(), ServiceError> {
    tokio::time::timeout(MAILBOX_FLAG_WAIT, async {
        loop {
            if *flag.borrow_and_update() {
                return Ok::<(), ()>(());
            }
            flag.changed().await.map_err(|_| ())?;
        }
    })
    .await
    .map_err(|_| service_failure(message))?
    .map_err(|()| service_failure("the mailbox rendezvous closed"))?;
    Ok(())
}

async fn wait_hook_flag(
    flag: &mut watch::Receiver<bool>,
    message: &'static str,
) -> Result<(), HookError> {
    tokio::time::timeout(MAILBOX_FLAG_WAIT, async {
        loop {
            if *flag.borrow_and_update() {
                return Ok::<(), ()>(());
            }
            flag.changed().await.map_err(|_| ())?;
        }
    })
    .await
    .map_err(|_| hook_failure(message))?
    .map_err(|()| hook_failure("the mailbox rendezvous closed"))?;
    Ok(())
}

fn service_failure(message: &str) -> ServiceError {
    ServiceError::failed(Some(Service::Agents), message)
}

fn hook_failure(message: impl Into<Box<str>>) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

/// Every mail record of one session's journal, parsed minimally.
fn journal_mail(data: &Path, session: &SessionId) -> support::TestResult<Vec<sonic_rs::Value>> {
    let dir = support::session_dir(data, &session.to_string())
        .ok_or_else(|| format!("no session journal found for {session}"))?;
    let text = fs::read_to_string(dir.join("journal.jsonl"))?;
    let mut records = Vec::new();
    for line in support::journal_records(&text, "mail") {
        records.push(sonic_rs::from_str(line)?);
    }
    Ok(records)
}

/// One string field of a journal mail record.
fn mail_field<'a>(
    mail: &'a sonic_rs::Value,
    key: &str,
) -> Result<&'a str, Box<dyn std::error::Error + Send + Sync>> {
    mail.get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("the mail journal record is missing field {key}").into())
}

/// Every `journal.jsonl` under `root`; the probe's session tree is small
/// and the walk is capped.
fn collect_journals(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if found.len() >= 4096 {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_journals(&path, found);
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the mailbox criterion covers fifo, cursor reads, both named refusals, and the drain leg in one host"
)]
fn mailbox_reads_are_cursor_based_and_full_or_gone_are_named() -> support::TestResult<()> {
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-mailbox-gate-{}", uuid::Uuid::now_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    let outside = root.0.join("outside");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    fs::create_dir_all(&outside)?;
    let fixture = data.join("mailbox-scripted.jsonl");
    let steps: Vec<String> = (0..4)
        .map(|index| support::text_step(&format!("ready {index}")))
        .collect();
    fs::write(&fixture, format!("{}\n", steps.join("\n")))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let state = Arc::new(MailboxProbe::new());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let factory = dalgona::product();
        let config = dal_core::Config::load(
            dal_core::ConfigProduct::Dalgona,
            &data,
            factory.defaults,
            Some(&user),
        )?;
        let cx = dalgon::BuildCx {
            data_root: data.clone(),
            config: &config,
        };
        let mut product = (factory.build)(&cx)?;
        for extension in &product.extensions {
            let services = extension.inject().capabilities();
            if services.is_empty() || extension.origin() == dal_core::Origin::Builtin {
                continue;
            }
            GrantStore::new(data.clone())
                .grant(
                    GrantKey::for_extension(extension, &workspace)
                        .expect("registered extension has a code identity"),
                    ClientId::new("mailbox-gate"),
                    Timestamp::now(),
                )
                .await?;
        }
        let extension =
            ExtensionBuilder::new("mailbox-gate", "0.1.0", ServiceSet::from_names(["agents"])?)?
                .with_origin(dal_core::Origin::User, None)
                .command(
                    dal_core::CommandSpec {
                        name: dal_core::CommandName::parse("mailbox-probe")?,
                        summary: "Exercise the member mailbox.".into(),
                        args_hint: None,
                    },
                    Arc::new(ProbeCommand {
                        state: Arc::clone(&state),
                    }),
                )
                .on_before_request(ProbeHook {
                    state: Arc::clone(&state),
                })
                .build()?;
        GrantStore::new(data.clone())
            .grant(
                GrantKey::for_extension(&extension, &workspace)
                    .ok_or("the probe extension has no code identity")?,
                ClientId::new("mailbox-gate"),
                Timestamp::now(),
            )
            .await?;
        product.extensions.push(extension);
        let env = Env {
            vars: BTreeMap::new(),
            cwd: workspace.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await?;
        let external_session = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(outside)?,
                    name: None,
                },
                ClientId::new("mailbox-gate"),
            )
            .await?;
        *shared_lock(&state.external) = Some(external_session.view(PageReq::default())?.session.id);
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace)?,
                    name: None,
                },
                ClientId::new("mailbox-gate"),
            )
            .await?;
        let reply = tokio::time::timeout(
            MAILBOX_PROBE_WAIT,
            agent.submit(dal_core::Command::Run {
                name: "mailbox-probe".into(),
                args: String::new().into(),
                expected: None,
            }),
        )
        .await??;
        assert!(matches!(reply, Reply::Done(_)));

        let (parent, member) = shared_lock(&state.sessions)
            .as_ref()
            .copied()
            .ok_or("the probe did not record its sessions")?;
        let capacity = shared_lock(&state.capacity)
            .as_ref()
            .copied()
            .ok_or("the probe did not record its capacity child")?;
        let external = shared_lock(&state.external)
            .as_ref()
            .copied()
            .ok_or("the out-of-tree session was not registered")?;
        let receipts = shared_lock(&state.receipts)
            .clone()
            .ok_or("the probe did not record its receipts")?;
        let reads = shared_lock(&state.reads)
            .clone()
            .ok_or("the member never recorded its mailbox reads")?;

        // FIFO receipts: the seed and aside confirm delivery, the steer
        // reaches the member's held turn, the next-turn message buffers.
        assert_eq!(
            receipts.initial,
            [Receipt::Delivered, Receipt::Delivered, Receipt::Delivered],
            "the seed and aside must confirm delivery and the steer must reach the held turn"
        );
        assert_eq!(receipts.third, Receipt::Buffered);
        assert_eq!(receipts.waiting.len(), MAILBOX_QUEUE_COUNT);
        assert!(
            receipts
                .waiting
                .iter()
                .all(|receipt| *receipt == Receipt::Buffered),
            "all 100 waiting NextTurn messages must be buffered"
        );
        // The named refusal on the 101st. The count behind it is a
        // retained-record bound, not a waiting-queue proof; the drain
        // receipt below carries the queue semantics.
        assert_eq!(receipts.full, Receipt::Full);
        assert_eq!(receipts.external, Receipt::Gone);
        assert_eq!(receipts.finished, Receipt::Gone);

        // Cursor reads: the first walk sees the seed, two walks from the
        // same cursor agree, and no read consumes anything.
        assert_eq!(reads.first.len(), 1);
        assert_eq!(reads.first[0].text.as_ref(), "cursor-seed");
        assert_eq!(reads.first[0].mode, MailMode::Aside);
        assert!(
            reads
                .first
                .iter()
                .all(|mail| mail.from == parent && mail.to == member && mail.reply_to.is_none())
        );
        let seed_cursor = reads
            .first_next
            .ok_or("the member's first read returned no cursor")?;
        assert_eq!(
            reads.repeated_first, reads.repeated_second,
            "two reads from the same cursor must return identical messages"
        );
        assert_eq!(
            reads.repeated_first_next, reads.repeated_second_next,
            "two reads from the same cursor must return the same cursor"
        );
        let observed_cursor = reads
            .repeated_first_next
            .ok_or("the repeat read from the seed cursor returned no cursor")?;
        assert_ne!(
            seed_cursor, observed_cursor,
            "the cursor must advance as messages arrive"
        );
        assert_eq!(reads.repeated_first.len(), 3);
        assert_eq!(
            reads
                .repeated_first
                .iter()
                .map(|mail| mail.text.as_ref())
                .collect::<Vec<_>>(),
            ["aside-one", "steer-two", "next-turn-three"],
            "per-pair delivery is FIFO"
        );
        assert_eq!(
            reads
                .repeated_first
                .iter()
                .map(|mail| mail.mode)
                .collect::<Vec<_>>(),
            [MailMode::Aside, MailMode::Steer, MailMode::NextTurn]
        );
        assert!(
            reads.repeated_first.iter().all(|mail| {
                mail.from == parent && mail.to == member && mail.reply_to == Some(seed_cursor)
            }),
            "every mail must carry its exact fields including the reply pointer"
        );

        // The retained journal pins the exact records and proves the
        // named `Gone` refusals wrote nothing: the member keeps exactly
        // the four delivered records, and no journal anywhere holds a
        // record addressed to the out-of-tree session.
        let member_mail = journal_mail(&data, &member)?;
        assert_eq!(
            member_mail.len(),
            4,
            "the member journal must hold exactly the four delivered records"
        );
        let member_texts = member_mail
            .iter()
            .map(|mail| mail_field(mail, "text"))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            member_texts,
            ["cursor-seed", "aside-one", "steer-two", "next-turn-three"]
        );
        let member_modes = member_mail
            .iter()
            .map(|mail| mail_field(mail, "mode"))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(member_modes, ["aside", "aside", "steer", "next_turn"]);
        for mail in &member_mail {
            assert_eq!(mail_field(mail, "from")?, parent.to_string());
            assert_eq!(mail_field(mail, "to")?, member.to_string());
        }
        assert_eq!(
            member_mail[0]
                .get("reply_to")
                .and_then(|value| value.as_str()),
            None,
            "the seed carries no reply pointer"
        );
        let expected_reply = seed_cursor.to_string();
        for mail in &member_mail[1..] {
            assert_eq!(
                mail.get("reply_to").and_then(|value| value.as_str()),
                Some(expected_reply.as_str()),
                "every reply must carry the seed's journal pointer"
            );
        }
        let capacity_mail = journal_mail(&data, &capacity)?;
        assert!(
            capacity_mail.len() >= MAILBOX_QUEUE_COUNT,
            "the capacity journal must retain the 100 queued records"
        );
        let capacity_texts = capacity_mail
            .iter()
            .take(MAILBOX_QUEUE_COUNT)
            .map(|mail| mail_field(mail, "text").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let expected_queue: Vec<String> = (0..MAILBOX_QUEUE_COUNT)
            .map(|index| format!("queued-{index}"))
            .collect();
        assert_eq!(capacity_texts, expected_queue, "the queue is FIFO on disk");
        for mail in &capacity_mail {
            assert_ne!(
                mail_field(mail, "text")?,
                "queue-overflow",
                "a Full send must not create a journal record"
            );
        }
        let mut journals = Vec::new();
        collect_journals(&data.join("sessions"), &mut journals);
        assert!(!journals.is_empty(), "the host must keep session journals");
        for journal in journals {
            let text = fs::read_to_string(&journal)?;
            for line in support::journal_records(&text, "mail") {
                let record: sonic_rs::Value = sonic_rs::from_str(line)?;
                assert_ne!(
                    mail_field(&record, "to")?,
                    external.to_string(),
                    "the out-of-tree recipient must receive no journal record"
                );
            }
        }

        // Delivery-then-accept: at the drain turn's boundary the waiting
        // queue was deliverable, so a new `NextTurn` must be buffered
        // while the delivered records stay retained. Expected red until
        // the fold gains a typed mail-delivery record: the bound counts
        // every retained `NextTurn` row and no journaled delivery event
        // ever retires one, so waiting and delivered mail are
        // indistinguishable and the 101st is refused for its whole
        // session lifetime. Minimal typed repair: a fold-owned delivery
        // event over the delivered journal ordinals that the capacity
        // check reads, never a runtime counter.

        let report = host.shutdown(Duration::from_secs(10)).await;
        assert_eq!(report.tasks_remaining, 0);
        // Assert after shutdown so the expected fail-first result still
        // releases every owned host task and session before the panic.
        assert_eq!(
            receipts.drain,
            Receipt::Buffered,
            "a NextTurn send must be buffered once the waiting queue was \
             delivered at the recipient's turn boundary while the delivered \
             records stay retained; the host refused with Full because its \
             capacity bound counts retained rows, not waiting messages"
        );
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
