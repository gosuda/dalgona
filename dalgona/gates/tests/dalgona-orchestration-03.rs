// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: every checked orchestration constant the
//! code enforces appears verbatim in the live `dalgona://orchestration`
//! page. The rows are read from the battery's code, not from the page.
//!
//! The wake gate drives the composed product through the bundled
//! orchestration battery's own automatic delivery: 21 single-step runs, one
//! report per wake, so the twenty-first wake meets the fold's typed
//! twenty-wake cutoff, the battery pauses with its once-only notice, and one
//! user prompt delivers the held report and resets the count. The model
//! boundary is a real provider over the openai-chat wire answered by a
//! test-local loopback server on `127.0.0.1:0` (the
//! `dalgona-orchestration-09` body-answer pattern, file-local here): every
//! response derives from the parsed request body, so the parent and 22
//! child sessions never share a scripted step queue, and no credential or
//! non-loopback address exists anywhere.
use dalgona_gates::support;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::task::{JoinError, JoinSet};

#[test]
fn constants_page_states_the_enforced_values() -> support::TestResult<()> {
    let scratch = support::Scratch::new("orchestration-constants")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(scratch.path().to_path_buf()).await?;
        let doc = host.doc("dalgona://orchestration")?;
        let page = &*doc.text;
        for (name, value) in dalgona_batteries::orchestration::constant_pairs() {
            let row = format!("| {name} | {value} |");
            assert!(
                page.contains(&row),
                "orchestration page omits or contradicts constant {name:?} = {value:?}"
            );
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.tasks_remaining, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}

/// The child report tool: only a run's child session carries it, so the
/// model can tell a child request from a parent request by tool surface.
const REPORT_TOOL: &str = "orchestration__report";

/// The chat-capable loopback model route; the server answers any model name.
const WAKE_MODEL: &str = "openai/gpt-wake-1";

const WAKE_CLIENT: &str = "wake-gate";

/// The provider's client-side concurrency cap the server never sees exceeded.
const MAX_CONCURRENT: usize = 8;

/// Bound of accepted-but-not-yet-served connections.
const ACCEPT_BACKLOG: usize = 32;

/// Hard shed bound of tracked connection tasks in the server `JoinSet`.
const CONNECTION_CAP: usize = 64;

/// Runs 1..=21 saturate the twenty-wake cutoff; run 22 starts from a user
/// prompt after the explicit stop, so its ready report proves no wake may
/// follow the stop.
const RUNS: usize = 22;

/// The enforced fold cutoff: twenty wake-started turns in a row.
const WAKE_LIMIT: usize = 20;

/// Twenty quiet-round deliveries at the battery's 2 s quiet rule.
const RUNS_WAIT: Duration = Duration::from_secs(150);

const TURN_WAIT: Duration = Duration::from_secs(30);

const NOTICE_WAIT: Duration = Duration::from_secs(15);

/// Outlasts the battery's 2 s quiet rule plus its 250 ms tick, so any wake
/// that could still fire has fired before the assert reads the journal.
const SETTLE: Duration = Duration::from_secs(8);

/// Grace the host gets to close its sessions and tracked tasks.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long an owned task group may take to finish once its stop condition
/// holds; a task still running after it is a failure, not a hang.
const DRAIN_WAIT: Duration = Duration::from_secs(5);

/// The battery's once-only pause notice (T-35), emitted on the typed
/// `DenyReason::WakeLimit` refusal of the twenty-first automatic wake.
const PAUSE_NOTICE: &str = "dalgona stopped starting turns by itself after 20 in a row. Your next message delivers the rest.";

const STOP_REPLY: &str = "automatic turns: stopped. Only /continuation run starts them again.";

/// Delivered report bodies are `task N done`; the parent's context carries
/// one user message per delivered wake.
const DELIVERY_MARK: &str = "done";

/// The journal key a `wake_attempt` record carries its wake count under.
const COUNT_KEY: &str = "\"count\":";

/// The journal key a `wake_attempt` record carries its delivered jobs under.
const JOBS_KEY: &str = "\"jobs\":[";

/// A group of owned tasks whose results the owner joins and checks.
type TaskSet = JoinSet<support::TestResult<()>>;

#[test]
fn wake_limit_rejects_the_twenty_first_and_user_prompt_resets_it() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-wake-limit")?;
    let data = scratch.path().join("data");
    let workspace = scratch.path().join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(exercise_wake_gate(&data, &workspace))
}

/// The gate's one sequence on one rig: twenty accepted wakes and the typed
/// refusal of the twenty-first, one user prompt that resets the count, then
/// an explicit stop that no prompt restarts.
async fn exercise_wake_gate(data: &Path, workspace: &Path) -> support::TestResult<()> {
    let rig = WakeRig::start(data, workspace).await?;
    rig.run_to_cutoff().await?;
    rig.check_pause_notice_once().await?;
    rig.check_paused_battery().await?;
    rig.user_prompt_resets_count().await?;
    rig.explicit_stop_holds().await?;
    rig.finish().await
}

/// The loopback provider: one listener on an ephemeral loopback port, an
/// owned acceptor feeding a bounded channel, and one server task that owns
/// the connection `JoinSet`. The acceptor and the server live in `tasks`,
/// which `stop` joins after the acceptor is told to stop; dropping the
/// server aborts them.
struct WakeServer {
    port: u16,
    state: Arc<Loopback>,
    tasks: TaskSet,
    stop_accepting: Arc<tokio::sync::Notify>,
}

impl WakeServer {
    /// Binds the listener and starts the acceptor and the server task.
    async fn start() -> support::TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Loopback::default());
        let (sender, accepted) = tokio::sync::mpsc::channel(ACCEPT_BACKLOG);
        let stop_accepting = Arc::new(tokio::sync::Notify::new());
        let mut tasks = TaskSet::new();
        tasks.spawn(accept_connections(
            listener,
            sender,
            Arc::clone(&stop_accepting),
        ));
        tasks.spawn(serve_connections(accepted, Arc::clone(&state)));
        Ok(Self {
            port,
            state,
            tasks,
            stop_accepting,
        })
    }

    /// Tells the acceptor to stop, then lets the server task drain its
    /// bounded channel and join every connection; the first task failure is
    /// returned with its cause.
    async fn stop(&mut self) -> support::TestResult<()> {
        self.stop_accepting.notify_one();
        drain_within(&mut self.tasks, "the loopback server").await
    }
}

/// Accepts connections and feeds them to the server task until told to stop
/// or the server task is gone; a full channel applies backpressure to the
/// listener.
async fn accept_connections(
    listener: tokio::net::TcpListener,
    sender: tokio::sync::mpsc::Sender<tokio::net::TcpStream>,
    stop_accepting: Arc<tokio::sync::Notify>,
) -> support::TestResult<()> {
    loop {
        tokio::select! {
            () = stop_accepting.notified() => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|error| {
                    format!("the loopback listener failed to accept: {error}")
                })?;
                if sender.send(stream).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// Serves every accepted connection from one owned `JoinSet`, at most
/// `CONNECTION_CAP` at a time, and joins them all once the acceptor is gone.
async fn serve_connections(
    mut accepted: tokio::sync::mpsc::Receiver<tokio::net::TcpStream>,
    state: Arc<Loopback>,
) -> support::TestResult<()> {
    let mut connections = TaskSet::new();
    let mut outcome = Ok(());
    while let Some(stream) = accepted.recv().await {
        keep_first(
            &mut outcome,
            reap_tasks(&mut connections, CONNECTION_CAP - 1).await,
        );
        connections.spawn(serve_wake_loopback(stream, Arc::clone(&state)));
    }
    keep_first(&mut outcome, reap_tasks(&mut connections, 0).await);
    outcome
}

/// One joined task's outcome: its own result, or the cause it died of. No
/// task is aborted while it is joined, so a cancelled or panicked task is a
/// failure.
fn joined_outcome(joined: Result<support::TestResult<()>, JoinError>) -> support::TestResult<()> {
    match joined {
        Ok(ran) => ran,
        Err(error) => Err(format!("a task died before it finished: {error}").into()),
    }
}

/// Keeps the first failure: `next` replaces `first` only while `first` is ok.
fn keep_first(first: &mut support::TestResult<()>, next: support::TestResult<()>) {
    if first.is_ok() {
        *first = next;
    }
}

/// Joins finished tasks until at most `keep` remain and returns the first
/// failure with its cause.
async fn reap_tasks(tasks: &mut TaskSet, keep: usize) -> support::TestResult<()> {
    let mut first = Ok(());
    while tasks.len() > keep
        && let Some(joined) = tasks.join_next().await
    {
        keep_first(&mut first, joined_outcome(joined));
    }
    first
}

/// Joins every task of `tasks` within [`DRAIN_WAIT`]; `what` names the group
/// in the failure when a task outlives it.
async fn drain_within(tasks: &mut TaskSet, what: &str) -> support::TestResult<()> {
    match tokio::time::timeout(DRAIN_WAIT, reap_tasks(tasks, 0)).await {
        Ok(outcome) => outcome,
        Err(elapsed) => {
            Err(format!("{what} still had tasks running after {DRAIN_WAIT:?}: {elapsed}").into())
        }
    }
}

/// The delivery notices the session published, as `(kind, text)`. Every lock
/// is held for one statement and never across an await.
#[derive(Default)]
struct Notices(Mutex<Vec<(String, String)>>);

impl Notices {
    fn lock(&self) -> support::TestResult<MutexGuard<'_, Vec<(String, String)>>> {
        self.0
            .lock()
            .map_err(|error| format!("notices mutex poisoned: {error}").into())
    }

    fn push(&self, kind: &str, text: &str) -> support::TestResult<()> {
        self.lock()?.push((kind.to_owned(), text.to_owned()));
        Ok(())
    }

    fn snapshot(&self) -> support::TestResult<Vec<(String, String)>> {
        Ok(self.lock()?.clone())
    }

    fn arrived(&self) -> support::TestResult<usize> {
        Ok(self.lock()?.len())
    }

    /// Waits until `wanted` delivery notices have arrived.
    async fn wait_for(&self, wanted: usize, limit: Duration) -> support::TestResult<()> {
        let timeout_result = tokio::time::timeout(limit, self.until(wanted)).await;
        match timeout_result {
            Ok(arrived) => arrived,
            Err(elapsed) => {
                let seen = self.arrived()?;
                Err(format!("only {seen} of {wanted} delivery notices arrived: {elapsed}").into())
            }
        }
    }

    async fn until(&self, wanted: usize) -> support::TestResult<()> {
        while self.arrived()? < wanted {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }
}

/// Records every `orchestration.delivery` notice as `(kind, text)` the
/// session publishes, so the once-only pause notice is observable. The task
/// ends when the session closes the subscription.
async fn collect_delivery_notices(
    mut subscription: dal_agent::Subscription,
    notices: Arc<Notices>,
) -> support::TestResult<()> {
    while let Some(delivery) = subscription.next().await {
        if let dal_agent::Delivery::Update(update) = delivery
            && let dal_core::UpdateKind::Notice(notice) = &update.kind
            && notice.kind.as_ref() == "orchestration.delivery"
        {
            notices.push(&notice.kind, &notice.text)?;
        }
    }
    Ok(())
}

/// The gate's fixture: the real composed product on a real session, its
/// loopback provider, and the notice collector. Each spawned task belongs to
/// a `JoinSet` that `finish` joins; dropping the rig aborts them.
struct WakeRig {
    data: PathBuf,
    host: dal_agent::Host,
    agent: dal_agent::Agent,
    session: String,
    notices: Arc<Notices>,
    collectors: TaskSet,
    server: WakeServer,
}

impl WakeRig {
    /// Starts the loopback provider, grants the bundled extensions, starts
    /// the host with a provider routed at the loopback port, and opens the
    /// session the runs go through.
    async fn start(data: &Path, workspace: &Path) -> support::TestResult<Self> {
        let server = WakeServer::start().await?;
        let user = wake_user_config(server.port);
        support::grant_bundled_extensions(data, workspace, &user, WAKE_CLIENT).await?;
        let product = support::build_product(data.to_path_buf(), Some(&user))?;
        let config = support::host_config(data, &user)?;
        let host = dal_agent::Host::start(product, config, wake_env(data)).await?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(workspace.to_path_buf())?,
                    name: None,
                },
                dal_core::ClientId::new(WAKE_CLIENT),
            )
            .await?;
        let session = agent
            .view(dal_core::PageReq::default())?
            .session
            .id
            .to_string();
        let notices = Arc::new(Notices::default());
        let mut collectors = TaskSet::new();
        collectors.spawn(collect_delivery_notices(
            agent.subscribe_listen(None)?,
            Arc::clone(&notices),
        ));
        Ok(Self {
            data: data.to_path_buf(),
            host,
            agent,
            session,
            notices,
            collectors,
            server,
        })
    }

    /// The parent session's journal; empty until its file exists.
    fn journal(&self) -> support::TestResult<String> {
        Ok(read_journal(&self.data, &self.session)?.unwrap_or_default())
    }

    /// Submits one user prompt on the idle session.
    async fn prompt(&self, text: &str) -> support::TestResult<()> {
        self.agent
            .submit(dal_core::Command::Prompt {
                expect: dal_core::Expect::Idle,
                content: vec![dal_core::Part::Text { text: text.into() }],
            })
            .await?;
        Ok(())
    }

    /// The prompt turn starts run 1; runs 2..=`WAKE_LIMIT`+1 start from the
    /// wake turns the battery itself issues, one report per wake.
    async fn run_to_cutoff(&self) -> support::TestResult<()> {
        self.prompt("start the runs").await?;
        wait_for_journal(
            &self.data,
            &self.session,
            "twenty accepted wake turns after the prompt turn",
            |journal| {
                support::journal_records(journal, "turn_end").len() >= 21
                    && support::journal_records(journal, "wake_attempt").len() >= WAKE_LIMIT
            },
            RUNS_WAIT,
        )
        .await?;
        let journal = self.journal()?;
        let wakes = wake_facts(&journal)?;
        assert_eq!(
            wakes.len(),
            WAKE_LIMIT,
            "exactly twenty wakes may be accepted before the cutoff: {wakes:?}"
        );
        for (index, (count, jobs)) in wakes.iter().enumerate() {
            let expected = u32::try_from(index + 1).map_err(|error| {
                format!("wake index {index} does not fit a wake count: {error}")
            })?;
            assert_eq!(
                *count, expected,
                "accepted wake counts run 1..=20 in order: {wakes:?}"
            );
            assert_eq!(
                *jobs, 1,
                "each wake delivers its one ready report: {wakes:?}"
            );
        }
        for n in 1..=WAKE_LIMIT {
            assert_eq!(
                journal.matches(&format!("task {n} done")).count(),
                1,
                "report {n} reached the model exactly once"
            );
        }
        Ok(())
    }

    /// The twenty-first automatic wake is refused typed at the fold: the
    /// battery pauses and announces the pause exactly once on the delivery
    /// channel.
    async fn check_pause_notice_once(&self) -> support::TestResult<()> {
        self.notices.wait_for(1, NOTICE_WAIT).await?;
        let notices = self.notices.snapshot()?;
        assert_eq!(
            notices.len(),
            1,
            "the pause announces itself exactly once: {notices:?}"
        );
        assert_eq!(
            notices[0].0.as_str(),
            "orchestration.delivery",
            "the notice rides the delivery channel: {notices:?}"
        );
        assert_eq!(
            notices[0].1.as_str(),
            PAUSE_NOTICE,
            "the notice names the twenty-wake cutoff verbatim"
        );
        Ok(())
    }

    /// No record, turn, or second notice may follow the refusal, and the
    /// held report waits for the user.
    async fn check_paused_battery(&self) -> support::TestResult<()> {
        tokio::time::sleep(SETTLE).await;
        let journal = self.journal()?;
        let wakes = wake_facts(&journal)?;
        assert_eq!(
            wakes.len(),
            WAKE_LIMIT,
            "no wake_attempt may follow the refusal: {wakes:?}"
        );
        assert_eq!(
            support::journal_records(&journal, "turn_end").len(),
            21,
            "no turn may run while the battery is paused"
        );
        assert!(
            !journal.contains("task 21 done"),
            "the held report waits for the user: {journal}"
        );
        Ok(())
    }

    /// One user prompt resumes the battery and resets the count: the held
    /// report is delivered as the next automatic wake, count 1.
    async fn user_prompt_resets_count(&self) -> support::TestResult<()> {
        self.prompt("deliver what you held").await?;
        wait_for_journal(
            &self.data,
            &self.session,
            "the held report delivered as wake count 1",
            |journal| {
                support::journal_records(journal, "wake_attempt").len() >= 21
                    && support::journal_records(journal, "turn_end").len() >= 23
            },
            TURN_WAIT,
        )
        .await?;
        let journal = self.journal()?;
        let wakes = wake_facts(&journal)?;
        assert_eq!(
            wakes.len(),
            21,
            "the reset wake is the only new wake_attempt: {wakes:?}"
        );
        assert_eq!(wakes[20].0, 1, "the user prompt reset the wake count to 1");
        assert_eq!(
            wakes[20].1, 1,
            "the reset wake carries the one held report: {wakes:?}"
        );
        assert_eq!(
            journal.matches("task 21 done").count(),
            1,
            "the held report reached the model exactly once"
        );
        Ok(())
    }

    /// An explicit stop is not restarted by prompts: the next run's ready
    /// report stays held and no automatic turn or notice may follow.
    async fn explicit_stop_holds(&self) -> support::TestResult<()> {
        assert_eq!(
            run_command(&self.agent, "continuation", "stop").await?,
            STOP_REPLY
        );
        self.prompt("start one more run").await?;
        wait_for_journal(
            &self.data,
            &self.session,
            "run 22 started from the prompt turn",
            |journal| journal.matches("started run ").count() >= RUNS,
            TURN_WAIT,
        )
        .await?;
        tokio::time::sleep(SETTLE).await;
        let journal = self.journal()?;
        assert_eq!(
            support::journal_records(&journal, "wake_attempt").len(),
            21,
            "a stopped battery issues no wake even with a ready report"
        );
        assert_eq!(
            support::journal_records(&journal, "turn_end").len(),
            24,
            "three user turns and twenty wake turns, nothing else"
        );
        assert_eq!(
            journal.matches("started run ").count(),
            RUNS,
            "all 22 single-step runs started through the agents tool"
        );
        let notices = self.notices.snapshot()?;
        assert_eq!(notices.len(), 1, "the stop emits no second notice");
        assert!(
            any_journal_contains(&self.data, "task 22 done")?,
            "run 22's report is ready, so the stopped leg is not vacuous"
        );
        Ok(())
    }

    /// Owned shutdown: the host closes its sessions, which ends the notice
    /// subscription; then the loopback server stops accepting and joins
    /// every connection, and its counters are checked.
    async fn finish(self) -> support::TestResult<()> {
        let Self {
            host,
            mut collectors,
            mut server,
            ..
        } = self;
        let report = host.shutdown(SHUTDOWN_GRACE).await;
        assert_eq!(report.tasks_remaining, 0);
        drain_within(&mut collectors, "the notice collector").await?;
        server.stop().await?;
        assert!(
            server.state.peak_in_flight.load(Ordering::SeqCst) <= MAX_CONCURRENT,
            "the loopback server saw more than the client's concurrency cap"
        );
        assert_eq!(
            server.state.failures.load(Ordering::SeqCst),
            0,
            "every loopback connection served cleanly"
        );
        Ok(())
    }
}

/// The user config routing the `openai` provider at the loopback `port`.
fn wake_user_config(port: u16) -> String {
    format!(
        "model = \"{WAKE_MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n\n[providers.openai]\nbase_url = \"http://127.0.0.1:{port}\"\napi = \"openai_chat\"\nmax_concurrent_requests = {MAX_CONCURRENT}\n"
    )
}

/// The host environment: a placeholder key and no sandbox helper.
fn wake_env(data: &Path) -> dal_agent::Env {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert(
        std::ffi::OsString::from("OPENAI_API_KEY"),
        std::ffi::OsString::from("sk-test"),
    );
    dal_agent::Env {
        vars,
        cwd: data.to_path_buf(),
        sandbox_helper: None,
    }
}

/// The loopback server's counters: served chats, in-flight requests, and the
/// concurrency peak the client's `max_concurrent_requests` must bound.
#[derive(Default)]
struct Loopback {
    chats: AtomicUsize,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    failures: AtomicUsize,
}

/// Answers one chat request from the parsed body alone, so parent and
/// children share no scripted order: children report `done` once and
/// acknowledge; the parent starts the next single-step run from each
/// delivery wake it receives.
///
/// # Errors
/// Returns a child request that carries no `task N` prompt, or an
/// unparsable one: the report it would name cannot be made up. Also returns
/// an SSE body that cannot be written.
fn wake_response(body: &sonic_rs::Value) -> support::TestResult<String> {
    if chat_tools(body).iter().any(|tool| tool == REPORT_TOOL) {
        if chat_answered_tool(body, REPORT_TOOL) {
            return Ok(sse_text("ack"));
        }
        let task = chat_task_number(body)?
            .ok_or("a child request carries no `task N` prompt to report on")?;
        return sse_calls(&[(
            format!("c{task}"),
            REPORT_TOOL.to_owned(),
            report_call(task),
        )]);
    }
    let agents = chat_answered_tool_count(body, "agents");
    let last = chat_last_user(body);
    if agents == 0 {
        return sse_calls(&[("r1".to_owned(), "agents".to_owned(), run_call(1))]);
    }
    // The stop leg: run 22 starts from a user prompt while the battery is
    // stopped, so its ready report proves no wake may follow the stop.
    if agents == 21 && last.contains("start one more run") {
        return sse_calls(&[("r22".to_owned(), "agents".to_owned(), run_call(22))]);
    }
    if last.contains("deliver what you held") {
        return Ok(sse_text("continuing"));
    }
    if last.contains(DELIVERY_MARK) {
        let delivered = chat_delivery_count(body);
        if agents == delivered && delivered < 21 {
            let next = delivered + 1;
            return sse_calls(&[(format!("r{next}"), "agents".to_owned(), run_call(next))]);
        }
        return Ok(sse_text("all delivered"));
    }
    Ok(sse_text("noted"))
}

/// The `agents` run action for one single-item step named `task N`.
fn run_call(n: usize) -> String {
    format!(r#"{{"action":"run","steps":[{{"name":"task-{n}","prompt":"task {n}"}}]}}"#)
}

/// The child's `done` report naming its task, so each delivery is unique.
fn report_call(n: u32) -> String {
    format!(r#"{{"status":"done","report":"task {n} done"}}"#)
}

/// The `(count, carried job count)` of every `wake_attempt` record, in
/// journal order.
///
/// # Errors
/// Returns the record that carries no wake count.
fn wake_facts(journal: &str) -> support::TestResult<Vec<(u32, usize)>> {
    let mut facts = Vec::new();
    for record in support::journal_records(journal, "wake_attempt") {
        let count = wake_count(record)?;
        let jobs = wake_jobs(record).ok_or_else(|| {
            format!("wake_attempt record with an unterminated job list: {record}")
        })?;
        facts.push((count, jobs));
    }
    Ok(facts)
}

/// The wake count one `wake_attempt` record carries.
///
/// # Errors
/// Returns a record without a count, or one whose count is not a `u32`.
fn wake_count(record: &str) -> support::TestResult<u32> {
    let at = record
        .find(COUNT_KEY)
        .ok_or_else(|| format!("wake_attempt record without a count: {record}"))?;
    let digits: String = record[at + COUNT_KEY.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u32>().map_err(|error| {
        format!("wake_attempt count {digits:?} is not a u32 ({error}): {record}").into()
    })
}

/// How many jobs one `wake_attempt` record delivers. The journal omits an
/// empty job list, so a record without the key delivers none; a list the
/// record never closes is malformed and yields `None`.
fn wake_jobs(record: &str) -> Option<usize> {
    let Some(at) = record.find(JOBS_KEY) else {
        return Some(0);
    };
    let inner = &record[at + JOBS_KEY.len()..];
    let inner = &inner[..inner.find(']')?];
    Some(if inner.trim().is_empty() {
        0
    } else {
        inner.split(',').count()
    })
}

/// Runs a registered extension command and returns its text reply.
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

/// Waits until `ready` holds for the parent session's journal. The error
/// names what was awaited and keeps the last journal read.
async fn wait_for_journal(
    data: &Path,
    session: &str,
    awaited: &str,
    ready: impl Fn(&str) -> bool,
    limit: Duration,
) -> support::TestResult<()> {
    let mut last = String::new();
    let timeout_result =
        tokio::time::timeout(limit, poll_journal(data, session, &ready, &mut last)).await;
    match timeout_result {
        Ok(polled) => polled,
        Err(elapsed) => {
            Err(format!("the journal never showed {awaited:?} ({elapsed}):\n{last}").into())
        }
    }
}

/// Reads the parent session's journal into `last` every 200 ms until
/// `ready` holds for it.
async fn poll_journal(
    data: &Path,
    session: &str,
    ready: &impl Fn(&str) -> bool,
    last: &mut String,
) -> support::TestResult<()> {
    loop {
        *last = read_journal(data, session)?.unwrap_or_default();
        if ready(last.as_str()) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Whether any session journal under `data` carries `needle`.
fn any_journal_contains(data: &Path, needle: &str) -> support::TestResult<bool> {
    let mut journals = Vec::new();
    collect(&data.join("sessions"), &mut journals)?;
    for path in journals {
        if read_text(&path)?.is_some_and(|text| text.contains(needle)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reads the journal of the parent session, found by its session id; a child
/// session keeps its own journal beside it. `Ok(None)` until the journal
/// exists.
fn read_journal(data: &Path, session: &str) -> support::TestResult<Option<String>> {
    let mut journals = Vec::new();
    collect(&data.join("sessions"), &mut journals)?;
    let Some(path) = journals.into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == session)
    }) else {
        return Ok(None);
    };
    read_text(&path)
}

/// The text of a file the host may still be appending to: a record cut in
/// the middle of a character reads lossily instead of failing the read.
/// `Ok(None)` when the file is gone.
fn read_text(path: &Path) -> support::TestResult<Option<String>> {
    let bytes = present(fs::read(path))
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(bytes.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
}

/// Maps a missing file or directory to `None`; any other failure stays one.
fn present<T>(read: std::io::Result<T>) -> std::io::Result<Option<T>> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Collects every `journal.jsonl` under `dir`; a directory the host removed
/// meanwhile holds none.
fn collect(dir: &Path, found: &mut Vec<PathBuf>) -> support::TestResult<()> {
    let Some(entries) = present(fs::read_dir(dir))
        .map_err(|error| format!("cannot list {}: {error}", dir.display()))?
    else {
        return Ok(());
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read an entry of {}: {error}", dir.display()))?;
        let path = entry.path();
        let Some(kind) = present(entry.file_type())
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?
        else {
            continue;
        };
        if kind.is_dir() {
            collect(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
    Ok(())
}

/// Reads one HTTP request: the target line and the `content-length` body,
/// returning the stream for the reply (the orch-09 pattern, file-local).
async fn read_http_request(
    stream: tokio::net::TcpStream,
) -> support::TestResult<(String, Vec<u8>, tokio::net::TcpStream)> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("malformed request line: {line:?}"))?
        .to_owned();
    let mut length = 0usize;
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value
                .trim()
                .parse::<usize>()
                .map_err(|error| format!("invalid content-length {value:?}: {error}"))?;
        }
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok((target, body, reader.into_inner()))
}

/// Serves one loopback connection and tallies a failure on the state before
/// returning its cause to the server task that joins it.
async fn serve_wake_loopback(
    stream: tokio::net::TcpStream,
    state: Arc<Loopback>,
) -> support::TestResult<()> {
    let served = answer_wake_request(stream, &state).await;
    if served.is_err() {
        state.failures.fetch_add(1, Ordering::SeqCst);
    }
    served
}

/// Answers one request: one `openai_chat` SSE reply, `connection: close`, so
/// no request order is ever shared.
async fn answer_wake_request(
    stream: tokio::net::TcpStream,
    state: &Loopback,
) -> support::TestResult<()> {
    use tokio::io::AsyncWriteExt;
    let (target, body, mut stream) = read_http_request(stream).await?;
    if !target.contains("chat/completions") {
        let response = "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }
    let parsed: sonic_rs::Value = sonic_rs::from_slice(&body)
        .map_err(|error| format!("loopback request is not JSON: {error}"))?;
    state.chats.fetch_add(1, Ordering::SeqCst);
    let live = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak_in_flight.fetch_max(live, Ordering::SeqCst);
    let events = wake_response(&parsed);
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
    let events = events?;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
        events.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

/// The advertised tool function names of one chat request body.
fn chat_tools(body: &sonic_rs::Value) -> Vec<String> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let Some(tools) = body.get("tools").and_then(|tools| tools.as_array()) else {
        return Vec::new();
    };
    tools
        .iter()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_owned)
        })
        .collect()
}

/// The assistant tool calls of one chat request body, in order.
fn chat_calls(body: &sonic_rs::Value) -> Vec<(String, String)> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let mut calls = Vec::new();
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return calls;
    };
    for message in messages {
        if message.get("role").and_then(|role| role.as_str()) != Some("assistant") {
            continue;
        }
        let Some(invokes) = message.get("tool_calls").and_then(|calls| calls.as_array()) else {
            continue;
        };
        for invoke in invokes {
            let id = invoke
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or_default();
            let name = invoke
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|name| name.as_str())
                .unwrap_or_default();
            calls.push((id.to_owned(), name.to_owned()));
        }
    }
    calls
}

/// The tool-call ids one chat request body already answers.
fn chat_answered(body: &sonic_rs::Value) -> Vec<String> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let mut answered = Vec::new();
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return answered;
    };
    for message in messages {
        if message.get("role").and_then(|role| role.as_str()) != Some("tool") {
            continue;
        }
        if let Some(id) = message.get("tool_call_id").and_then(|id| id.as_str()) {
            answered.push(id.to_owned());
        }
    }
    answered
}

/// Whether one chat request body answers a call of `tool`.
fn chat_answered_tool(body: &sonic_rs::Value, tool: &str) -> bool {
    chat_answered_tool_count(body, tool) > 0
}

/// How many answered tool results the body carries for `tool`: the parent's
/// started-run count is its answered `agents` result count.
fn chat_answered_tool_count(body: &sonic_rs::Value, tool: &str) -> usize {
    let answered = chat_answered(body);
    chat_calls(body)
        .iter()
        .filter(|(id, name)| name == tool && answered.contains(id))
        .count()
}

/// The text of every user message of one chat request body, in order.
fn chat_user_texts(body: &sonic_rs::Value) -> Vec<String> {
    use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};
    let Some(messages) = body
        .get("messages")
        .and_then(|messages| messages.as_array())
    else {
        return Vec::new();
    };
    messages
        .iter()
        .filter(|message| message.get("role").and_then(|role| role.as_str()) == Some("user"))
        .filter_map(|message| {
            let content = message.get("content");
            if let Some(text) = content.and_then(|content| content.as_str()) {
                return Some(text.to_owned());
            }
            let parts = content.and_then(|content| content.as_array())?;
            Some(
                parts
                    .iter()
                    .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })
        .collect()
}

/// The most recent user message text of one chat request body.
fn chat_last_user(body: &sonic_rs::Value) -> String {
    chat_user_texts(body).pop().unwrap_or_default()
}

/// How many user messages carry a delivered report (every run notice holds
/// the report body, which ends in `done`; the prompts never do).
fn chat_delivery_count(body: &sonic_rs::Value) -> usize {
    chat_user_texts(body)
        .iter()
        .filter(|text| text.contains(DELIVERY_MARK))
        .count()
}

/// The `task N` number in the request's user texts: the child's step prompt.
///
/// # Errors
/// Returns a number that does not fit a `u32`.
fn chat_task_number(body: &sonic_rs::Value) -> support::TestResult<Option<u32>> {
    let mut found = None;
    for text in chat_user_texts(body) {
        let mut rest = text.as_str();
        while let Some(at) = rest.find("task ") {
            let after = &rest[at + "task ".len()..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                let number = digits
                    .parse::<u32>()
                    .map_err(|error| format!("task number {digits:?} is not a u32: {error}"))?;
                found = Some(number);
            }
            rest = &after[digits.len()..];
        }
    }
    Ok(found)
}

/// One text-only SSE response (the orch-09 wire shape).
fn sse_text(text: &str) -> String {
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{}}}}}]}}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n",
        support::json_text(text)
    )
}

/// One SSE response calling every tool in `calls` (id, wire name, args).
///
/// # Errors
/// Returns a formatting failure while writing an event.
fn sse_calls(calls: &[(String, String, String)]) -> support::TestResult<String> {
    let mut events = String::new();
    for (index, (id, name, args)) in calls.iter().enumerate() {
        write!(
            events,
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":{},\"function\":{{\"name\":{},\"arguments\":{}}}}}]}}}}]}}\n\n",
            support::json_text(id),
            support::json_text(name),
            support::json_text(args)
        )?;
    }
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    write!(
        events,
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n"
    )?;
    Ok(events)
}
