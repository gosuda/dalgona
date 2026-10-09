// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Cancellation and settlement of child sessions, driven through the real
//! session owner against a scripted host.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, HookCx, RawValue, Services, Tool, ToolCx, ToolOutcome,
};
use dal_core::ext::{
    McpDeclaration, McpRequest, McpResponse, SessionEnd, SessionStart, Visibility,
};
use dal_core::{
    AgentInfo, AgentReport, AgentStart, AgentState, AgentsOp, AgentsReply, Answer, ArtifactFile,
    CallId, EntryId, FetchRequest, FetchResponse, Inference, JobId, JobStateView, JobStatus,
    JobsOp, JobsReply, ModelRequest, Notice, Question, RawJson, RunOutput, RunRequest, SessionId,
    SidecarOp, StateError, StateOp, StateRecord, Stop, TurnOp, TurnOpReply,
};
use tokio_util::sync::CancellationToken;

use super::Runtime;
use crate::orchestration::parse_config;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CLOSE_REFUSED: &str = "the host refused to close the session";

#[derive(Default)]
struct Script {
    lists: VecDeque<Result<Vec<AgentInfo>, &'static str>>,
    cancel_refused: HashSet<SessionId>,
    cancel_unexpected: HashSet<SessionId>,
    turn_idle_error: Option<&'static str>,
    jobs_list_error: Option<&'static str>,
    started: Option<SessionId>,
    await_error: Option<&'static str>,
    report: Option<String>,
    cancels: Vec<SessionId>,
    notices: Vec<Notice>,
    jobs: Vec<(JobStatus, Option<JobId>, String)>,
    starts: Vec<(SessionId, AgentStart)>,
    report_status: Option<&'static str>,
    hold: Option<CancellationToken>,
    run_outputs: VecDeque<Result<RunOutput, &'static str>>,
    run_requests: Vec<RunRequest>,
    artifacts: Vec<(JobId, ArtifactFile, Vec<u8>)>,
    wakes: Vec<String>,
    taken: HashSet<JobId>,
    delivered: HashSet<JobId>,
    lines: VecDeque<dal_core::JobLines>,
    line_reads: Vec<Option<u64>>,
    /// A refusal the host answers every start with.
    refuse_start: Option<dal_core::AgentRefusal>,
    /// Whether awaited children end without storing a report.
    silent: bool,
    /// Grace prompts the host accepted, in order.
    prompts: Vec<(SessionId, String)>,
    /// Await calls served so far; each later one reports a new entry.
    awaits: u64,
}

/// A host that answers only the services a session owner uses when it
/// cancels children; every other service reports itself unavailable.
#[derive(Default)]
struct Host {
    script: Mutex<Script>,
    this: Weak<Host>,
    runtime: Mutex<Option<(Runtime, SessionId)>>,
    changed: tokio::sync::Notify,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async {
        Err(ServiceError::failed(
            None,
            "unavailable in the scripted host",
        ))
    })
}

impl Host {
    fn cancels(&self) -> Vec<SessionId> {
        locked(&self.script).cancels.clone()
    }

    fn prompts(&self) -> Vec<(SessionId, String)> {
        locked(&self.script).prompts.clone()
    }

    fn notices(&self) -> Vec<Notice> {
        locked(&self.script).notices.clone()
    }

    fn start_child(&self, start: AgentStart, id: SessionId) -> ServiceFuture<'_, AgentsReply> {
        let binding = locked(&self.runtime).clone();
        let services: Option<Arc<dyn Services>> =
            self.this.upgrade().map(|host| host as Arc<dyn Services>);
        Box::pin(async move {
            if let (Some((runtime, parent)), Some(services)) = (binding, services) {
                let workspace = start.workspace.as_ref().map_or_else(
                    || ToolCx::for_test(Arc::clone(&services)).workspace().clone(),
                    Clone::clone,
                );
                let mut cx = HookCx::for_test(services, id, None);
                cx.parent = Some(parent);
                runtime
                    .open(
                        SessionStart {
                            session: id,
                            workspace,
                            resumed: false,
                        },
                        cx,
                    )
                    .await
                    .map_err(|error| ServiceError::failed(None, error.to_string()))?;
            }
            Ok(AgentsReply::Started { id })
        })
    }

    fn wait_job(&self, id: JobId) -> ServiceFuture<'_, JobsReply> {
        Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                if let Some(outcome) = locked(&self.script)
                    .jobs
                    .iter()
                    .find(|(job, _, _)| job.id == id)
                    .and_then(|(job, _, _)| match &job.state {
                        JobStateView::Done(outcome) => Some(outcome.clone()),
                        _ => None,
                    })
                {
                    return Ok(JobsReply::Waited { id, outcome });
                }
                changed.await;
            }
        })
    }

    fn take_reports(script: &mut Script, limit: u16) -> JobsReply {
        let reports: Vec<_> = script
            .jobs
            .iter()
            .filter(|(job, parent, _)| {
                parent.is_none()
                    && !script.taken.contains(&job.id)
                    && !script.delivered.contains(&job.id)
            })
            .filter_map(|(job, _, text)| match &job.state {
                JobStateView::Done(outcome) => Some(dal_core::JobReport {
                    id: job.id,
                    label: job.label.clone(),
                    outcome: outcome.clone(),
                    text: text.clone().into(),
                }),
                _ => None,
            })
            .take(usize::from(limit))
            .collect();
        script.taken.extend(reports.iter().map(|report| report.id));
        JobsReply::Taken(reports)
    }
}

impl Services for Host {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable()
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable()
    }

    fn run(&self, _who: &Caller, req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        let mut script = locked(&self.script);
        script.run_requests.push(req);
        let reply = script.run_outputs.pop_front();
        Box::pin(async move {
            reply
                .ok_or_else(|| ServiceError::failed(None, "no git output was scripted"))?
                .map_err(|message| ServiceError::failed(None, message))
        })
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unavailable()
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable()
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unavailable()
    }

    fn agents(&self, _who: &Caller, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        let mut script = locked(&self.script);
        let reply = match op {
            AgentsOp::List => match script.lists.pop_front() {
                Some(Ok(agents)) => Ok(AgentsReply::Listed(agents)),
                Some(Err(message)) => Err(ServiceError::failed(None, message)),
                None => Ok(AgentsReply::Listed(Vec::new())),
            },
            AgentsOp::Cancel { id } => {
                script.cancels.push(id);
                if script.cancel_unexpected.remove(&id) {
                    Ok(AgentsReply::Pending { id })
                } else if script.cancel_refused.contains(&id) {
                    Err(ServiceError::failed(None, CLOSE_REFUSED))
                } else {
                    Ok(AgentsReply::Cancelled { id })
                }
            }
            AgentsOp::Start(_) if script.refuse_start.is_some() => {
                let reason = script.refuse_start.clone();
                drop(script);
                return Box::pin(async move {
                    reason
                        .map(|reason| AgentsReply::Refused { reason })
                        .ok_or_else(|| ServiceError::failed(None, "no refusal was scripted"))
                });
            }
            AgentsOp::Start(start) => {
                let id = script.started.unwrap_or_else(SessionId::new_v7);
                script.starts.push((id, start.clone()));
                drop(script);
                self.changed.notify_waiters();
                return self.start_child(start, id);
            }
            AgentsOp::Prompt { id, text, .. } => {
                script.prompts.push((id, text.to_string()));
                Ok(AgentsReply::Prompted { id })
            }
            AgentsOp::Await { id, .. } => {
                script.awaits += 1;
                let entry = EntryId::new(
                    std::num::NonZeroU64::new(script.awaits).expect("an await count above zero"),
                );
                let (error, text, status, hold, silent) = (
                    script.await_error,
                    script.report.clone(),
                    script.report_status,
                    script.hold.clone(),
                    script.silent,
                );
                drop(script);
                let binding = locked(&self.runtime).clone();
                let services: Option<Arc<dyn Services>> =
                    self.this.upgrade().map(|host| host as Arc<dyn Services>);
                return Box::pin(async move {
                    if let Some(hold) = hold {
                        hold.cancelled().await;
                    }
                    if let Some(message) = error {
                        return Err(ServiceError::failed(None, message));
                    }
                    let text = text.unwrap_or_else(|| "scan finished".to_owned());
                    if !silent && let (Some((runtime, _)), Some(services)) = (binding, services) {
                        let caller = ToolCx::for_test(services).caller().clone();
                        let args = sonic_rs::to_string(&sonic_rs::json!({
                            "status": status.unwrap_or("done"), "report": text,
                        }))
                        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                        runtime
                            .tool(
                                id,
                                caller,
                                CallId::new("report"),
                                "report",
                                RawJson::parse(&args).map_err(|error| {
                                    ServiceError::failed(None, error.to_string())
                                })?,
                                CancellationToken::new(),
                            )
                            .await?;
                    }
                    Ok(AgentsReply::Await {
                        report: AgentReport {
                            stop: Stop::EndTurn,
                            text: text.into(),
                            session: id,
                            entry,
                        },
                    })
                });
            }
            _ => Err(ServiceError::failed(
                None,
                "unavailable in the scripted host",
            )),
        };
        Box::pin(async move { reply })
    }

    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        if let JobsOp::Wait { id, .. } = op {
            return self.wait_job(id);
        }
        let mut script = locked(&self.script);
        let reply = match op {
            JobsOp::Spawn { parent, name, .. } => {
                let id = JobId::new_v7();
                script.jobs.push((
                    JobStatus {
                        id,
                        label: name.to_string().into(),
                        state: JobStateView::Running,
                        log: None,
                        last_activity_at: dal_core::Timestamp::now(),
                    },
                    parent,
                    String::new(),
                ));
                Ok(JobsReply::Spawned { id })
            }
            JobsOp::Settle { id, outcome, text } => {
                let row = script
                    .jobs
                    .iter_mut()
                    .find(|(job, _, _)| job.id == id)
                    .ok_or_else(|| ServiceError::failed(None, "unknown job"));
                row.and_then(|(job, _, stored)| {
                    if matches!(job.state, JobStateView::Done(_)) {
                        return Err(ServiceError::failed(None, "job already ended"));
                    }
                    job.state = JobStateView::Done(outcome);
                    *stored = text.into();
                    Ok(JobsReply::Settled { id })
                })
            }
            JobsOp::Cancel { id } => {
                if let Some((job, _, _)) = script.jobs.iter_mut().find(|(job, _, _)| job.id == id) {
                    job.state = JobStateView::Done(dal_core::JobOutcome::Cancelled);
                }
                Ok(JobsReply::Cancelled { id })
            }
            JobsOp::Text { id } => Ok(JobsReply::Text {
                id,
                text: script
                    .jobs
                    .iter()
                    .find(|(job, _, _)| job.id == id)
                    .map_or_else(String::new, |(_, _, text)| text.clone())
                    .into(),
            }),
            JobsOp::Find { id } => Ok(JobsReply::Found(
                script
                    .jobs
                    .iter()
                    .find(|(job, _, _)| job.id == id)
                    .map(|(job, _, _)| job.clone()),
            )),
            JobsOp::List => match script.jobs_list_error.take() {
                Some(message) => Err(ServiceError::failed(None, message)),
                None => Ok(JobsReply::Listed(
                    script.jobs.iter().map(|(job, _, _)| job.clone()).collect(),
                )),
            },
            JobsOp::Lines { after, .. } => {
                script.line_reads.push(after);
                let mut lines = script.lines.front().cloned().unwrap_or_default();
                lines.lines.retain(|line| line.seq > after.unwrap_or(0));
                Ok(JobsReply::Lines(lines))
            }
            JobsOp::Take { limit } => Ok(Self::take_reports(&mut script, limit)),
            JobsOp::Commit { ids } => {
                script.delivered.extend(ids.iter().copied());
                for id in &ids {
                    script.taken.remove(id);
                }
                Ok(JobsReply::Committed { ids })
            }
            JobsOp::Release { ids } => {
                for id in &ids {
                    script.taken.remove(id);
                }
                Ok(JobsReply::Released { ids })
            }
            JobsOp::Hold { .. } | JobsOp::Unhold { .. } => Ok(JobsReply::Held(Vec::new())),
            _ => Err(ServiceError::failed(None, "unscripted job operation")),
        };
        drop(script);
        self.changed.notify_waiters();
        Box::pin(async move { reply })
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
        unavailable()
    }

    fn turn(&self, _who: &Caller, op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        match op {
            TurnOp::IsIdle => {
                let mut script = locked(&self.script);
                let reply = match script.turn_idle_error.take() {
                    Some(message) => Err(ServiceError::failed(None, message)),
                    None => Ok(TurnOpReply::Idle(true)),
                };
                Box::pin(async move { reply })
            }
            TurnOp::Cancel => Box::pin(async { Ok(TurnOpReply::Cancelled) }),
            TurnOp::Wake { text, .. } => {
                locked(&self.script).wakes.push(text.into());
                self.changed.notify_waiters();
                Box::pin(async { Ok(TurnOpReply::Woken) })
            }
            _ => unavailable(),
        }
    }
    fn sidecar(&self, _who: &Caller, op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        if let SidecarOp::Artifact { job, file, bytes } = op {
            locked(&self.script).artifacts.push((job, file, bytes));
        }
        Box::pin(async { Ok(None) })
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unavailable()
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unavailable()
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable()
    }

    fn notify(&self, _who: &Caller, notice: Notice) {
        locked(&self.script).notices.push(notice);
    }

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unavailable()
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unavailable()
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
}

struct Fixture {
    runtime: Runtime,
    host: Arc<Host>,
    session: SessionId,
}

impl Fixture {
    /// Opens with every battery except the goal owner, for the tests that
    /// drive cancellation and runs directly.
    async fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = parse_config(None)?;
        config.goal.enabled = false;
        Self::open_owner(config).await
    }

    /// Opens with the goal owner enabled, for the continuation tests.
    async fn open_goal() -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_owner(parse_config(None)?).await
    }

    async fn open_config(
        mut config: crate::orchestration::OrchestrationConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        config.goal.enabled = false;
        Self::open_owner(config).await
    }

    async fn open_owner(
        config: crate::orchestration::OrchestrationConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let runtime = Runtime::new(config)?;
        let host = Arc::new_cyclic(|this| Host {
            this: this.clone(),
            ..Host::default()
        });
        let session = SessionId::new_v7();
        *locked(&host.runtime) = Some((runtime.clone(), session));
        let services: Arc<dyn Services> = host.clone();
        let workspace = ToolCx::for_test(Arc::clone(&services)).workspace().clone();
        let cx = HookCx::for_test(services, session, None);
        runtime
            .open(
                SessionStart {
                    session,
                    workspace,
                    resumed: false,
                },
                cx,
            )
            .await?;
        let fixture = Self {
            runtime,
            host,
            session,
        };
        fixture.user_input().await?;
        Ok(fixture)
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        // Tests control the host replies, not the owner's state.
        locked(&self.host.script)
    }

    fn caller(&self) -> Caller {
        ToolCx::for_test(self.host.clone()).caller().clone()
    }

    async fn tool(&self, args: &str) -> Result<String, ServiceError> {
        self.runtime
            .tool(
                self.session,
                self.caller(),
                CallId::new("call"),
                "agents",
                RawJson::parse(args)
                    .map_err(|error| ServiceError::failed(None, error.to_string()))?,
                CancellationToken::new(),
            )
            .await
    }

    async fn ended_run(&self) -> Result<String, ServiceError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let changed = self.host.changed.notified();
                if let Some(text) = self
                    .script()
                    .jobs
                    .iter()
                    .find(|(job, parent, _)| {
                        parent.is_none() && matches!(job.state, JobStateView::Done(_))
                    })
                    .map(|(_, _, text)| text.clone())
                {
                    return text;
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| ServiceError::failed(None, "the run did not settle"))
    }

    async fn user_input(&self) -> TestResult {
        use dal_agent::ext::Hook as _;
        super::InputHook(self.runtime.clone())
            .call(
                dal_core::ext::InputEvent {
                    content: vec![dal_core::Part::Text {
                        text: "work".into(),
                    }],
                },
                HookCx::for_test(self.host.clone(), self.session, None),
            )
            .await?;
        Ok(())
    }

    /// Creates the session goal through the `/goal` command.
    async fn create_goal(&self, objective: &str) -> TestResult {
        self.runtime
            .command(self.session, "goal", objective)
            .await?;
        Ok(())
    }

    /// Opens one user prompt turn: the input hook, then the before-turn hook.
    async fn begin_turn(&self, turn: dal_core::TurnId) -> TestResult {
        use dal_agent::ext::Hook as _;
        super::BeforeTurnHook(self.runtime.clone())
            .call(
                dal_core::ext::BeforeTurn {
                    turn,
                    text: "work".into(),
                },
                HookCx::for_test(self.host.clone(), self.session, Some(turn)),
            )
            .await?;
        Ok(())
    }

    /// Ends the active turn: the turn-end hook, then the settled hook.
    async fn end_turn(&self, turn: dal_core::TurnId, stop: Stop, reply_text: &str) -> TestResult {
        use dal_agent::ext::ObserveHook as _;
        super::TurnEndHook(self.runtime.clone())
            .call(
                dal_core::ext::TurnEnd { turn, stop },
                HookCx::for_test(self.host.clone(), self.session, Some(turn)),
            )
            .await?;
        super::SettledHook(self.runtime.clone())
            .call(
                dal_core::ext::Settled {
                    turn,
                    reply_text: reply_text.into(),
                },
                HookCx::for_test(self.host.clone(), self.session, Some(turn)),
            )
            .await?;
        Ok(())
    }

    /// Runs one full user prompt turn to its settled end.
    async fn user_turn(&self, turn: dal_core::TurnId, stop: Stop, reply_text: &str) -> TestResult {
        self.user_input().await?;
        self.begin_turn(turn).await?;
        self.end_turn(turn, stop, reply_text).await
    }

    /// Ends one top-level job so its report waits for the next wake.
    fn done_job(&self, text: &str) -> JobId {
        let job = JobId::new_v7();
        self.script().jobs.push((
            JobStatus {
                id: job,
                label: "report".into(),
                state: JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
                log: None,
                last_activity_at: dal_core::Timestamp::now(),
            },
            None,
            text.to_owned(),
        ));
        job
    }

    /// Lets the session owner run the timer and tick work that virtual time
    /// made ready.
    async fn pump(&self) {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        locked(&self.host.runtime).take();
    }
}

fn agent(id: SessionId, state: AgentState) -> AgentInfo {
    AgentInfo {
        id,
        name: "child".into(),
        state,
    }
}

#[tokio::test]
async fn abort_cancels_every_descendant_once_and_continues_past_a_failure() -> TestResult {
    let fixture = Fixture::open().await?;
    let (first, refused, queued, nested) = (
        SessionId::new_v7(),
        SessionId::new_v7(),
        SessionId::new_v7(),
        SessionId::new_v7(),
    );
    {
        let mut script = fixture.script();
        script.cancel_refused.insert(refused);
        script.lists.push_back(Ok(vec![
            agent(first, AgentState::Running),
            agent(refused, AgentState::Running),
            agent(queued, AgentState::Queued),
        ]));
        // The host now reveals a deeper descendant, and still lists the
        // children that were already handled.
        script.lists.push_back(Ok(vec![
            agent(first, AgentState::Running),
            agent(refused, AgentState::Running),
            agent(queued, AgentState::Queued),
            agent(nested, AgentState::Running),
        ]));
    }
    let reply = fixture
        .runtime
        .command(fixture.session, "abort", "")
        .await?;
    assert_eq!(
        fixture.host.cancels(),
        vec![first, refused, queued, nested],
        "every descendant is cancelled exactly once"
    );
    assert!(reply.starts_with("aborted:"), "reply: {reply}");
    assert!(
        reply.contains(&refused.to_string()) && reply.contains(CLOSE_REFUSED),
        "the failed child is named with its cause: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn abort_still_completes_when_the_child_list_fails() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture
        .script()
        .lists
        .push_back(Err("the child list is unavailable"));
    let reply = fixture
        .runtime
        .command(fixture.session, "abort", "")
        .await?;
    assert!(reply.starts_with("aborted:"), "reply: {reply}");
    assert!(
        reply.contains("the child list is unavailable"),
        "the list failure is reported: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn session_end_cancels_every_descendant_and_reports_failures() -> TestResult {
    let fixture = Fixture::open().await?;
    let (refused, nested) = (SessionId::new_v7(), SessionId::new_v7());
    {
        let mut script = fixture.script();
        script.cancel_refused.insert(refused);
        script
            .lists
            .push_back(Ok(vec![agent(refused, AgentState::Running)]));
        script.lists.push_back(Ok(vec![
            agent(refused, AgentState::Running),
            agent(nested, AgentState::Running),
        ]));
    }
    fixture
        .runtime
        .close(
            fixture.session,
            SessionEnd {
                session: fixture.session,
                reason: "close".into(),
            },
        )
        .await;
    assert_eq!(fixture.host.cancels(), vec![refused, nested]);
    let notices = fixture.host.notices();
    assert_eq!(notices.len(), 1, "one notice reports the failure");
    assert!(
        notices[0].text.contains(&refused.to_string()) && notices[0].text.contains(CLOSE_REFUSED),
        "notice: {}",
        notices[0].text
    );
    Ok(())
}

async fn run_one_step(fixture: &Fixture) -> Result<String, ServiceError> {
    let reply = fixture.tool(
        r#"{"action":"run","steps":[{"name":"scan","prompt":"look","tools":["read"],"isolation":"shared"}]}"#,
    ).await?;
    assert!(reply.starts_with("started run "), "{reply}");
    fixture.ended_run().await
}

#[tokio::test]
async fn failed_wait_cancels_the_child_and_keeps_both_failures() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.await_error = Some("the provider stopped answering");
        script.cancel_refused.insert(child);
    }
    let error = run_one_step(&fixture).await?;
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(
        error.contains("the provider stopped answering") && error.contains(CLOSE_REFUSED),
        "both failures stay visible: {error}"
    );
    Ok(())
}

#[tokio::test]
async fn failed_wait_with_clean_teardown_keeps_only_the_result_failure() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.await_error = Some("the provider stopped answering");
    }
    let error = run_one_step(&fixture).await?;
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(error.contains("the provider stopped answering"), "{error}");
    assert!(!error.contains(CLOSE_REFUSED), "{error}");
    Ok(())
}

#[tokio::test]
async fn a_refused_child_start_shows_the_exact_reason_in_the_run() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().refuse_start = Some(dal_core::AgentRefusal::MaxDepth { max_depth: 1 });
    let reply = run_one_step(&fixture).await?;
    assert!(
        reply.contains("child sessions cannot start children here: agents.max_depth = 1."),
        "the refusal text reaches the run report: {reply}"
    );
    assert!(fixture.host.prompts().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_silent_child_shows_its_last_message_with_the_failure() -> TestResult {
    let fixture = Fixture::open().await?;
    {
        let mut script = fixture.script();
        script.started = Some(SessionId::new_v7());
        script.silent = true;
        script.report = Some("tried hard".into());
    }
    let reply = run_one_step(&fixture).await?;
    assert!(
        reply.contains("no report after the last turn")
            && reply.contains("last message (not a report): tried hard"),
        "the failure shows the last message: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn a_silent_child_with_no_text_shows_no_last_message() -> TestResult {
    let fixture = Fixture::open().await?;
    {
        let mut script = fixture.script();
        script.started = Some(SessionId::new_v7());
        script.silent = true;
        script.report = Some(String::new());
    }
    let reply = run_one_step(&fixture).await?;
    assert!(reply.contains("no report after the last turn"), "{reply}");
    assert!(
        !reply.contains("last message"),
        "nothing extra shows: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn a_child_without_a_report_gets_one_grace_turn_then_fails() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.silent = true;
    }
    let reply = run_one_step(&fixture).await?;
    assert!(
        reply.contains("no report after the last turn"),
        "silence after grace fails the task: {reply}"
    );
    assert_eq!(
        fixture.host.prompts(),
        vec![(
            child,
            crate::orchestration::pool::grace_text(
                crate::orchestration::pool::GraceCause::NoReport
            )
        )],
        "the silent child earned exactly one grace prompt"
    );
    assert_eq!(
        fixture.host.cancels(),
        vec![child],
        "the settled child is released after its grace turn"
    );
    Ok(())
}

#[tokio::test]
async fn a_reported_child_is_released_with_no_client_attached() -> TestResult {
    // The scripted host has no connected client, like a run that outlives
    // the front end that started it. The report must still release the
    // child runtime; nothing else would ever close it.
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.report = Some("scan finished".into());
    }
    let reply = run_one_step(&fixture).await?;
    assert!(
        reply.contains("scan finished"),
        "the report is kept: {reply}"
    );
    assert_eq!(
        fixture.host.cancels(),
        vec![child],
        "the reported child is released once"
    );
    assert!(fixture.host.notices().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_refused_release_keeps_the_report_and_tells_the_user() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.report = Some("scan finished".into());
        script.cancel_refused.insert(child);
    }
    let reply = run_one_step(&fixture).await?;
    assert!(
        reply.contains("scan finished"),
        "the report is kept: {reply}"
    );
    let notices = fixture.host.notices();
    assert_eq!(notices.len(), 1, "one notice reports the failed release");
    assert!(
        notices[0].text.contains(&child.to_string()) && notices[0].text.contains(CLOSE_REFUSED),
        "notice: {}",
        notices[0].text
    );
    Ok(())
}

#[tokio::test]
async fn cancel_action_continues_past_a_failing_child() -> TestResult {
    let fixture = Fixture::open().await?;
    let (refused, healthy) = (SessionId::new_v7(), SessionId::new_v7());
    fixture.script().cancel_refused.insert(refused);
    let args = format!("{{\"action\":\"cancel\",\"ids\":[\"{refused}\",\"{healthy}\"]}}");
    let error = fixture
        .runtime
        .tool(
            fixture.session,
            fixture.caller(),
            CallId::new("call"),
            "agents",
            RawJson::parse(&args)?,
            CancellationToken::new(),
        )
        .await
        .err()
        .ok_or("a refused cancel must be reported")?
        .to_string();
    assert_eq!(
        fixture.host.cancels(),
        vec![refused, healthy],
        "the healthy child is cancelled after the refused one"
    );
    assert!(
        error.contains("cancelled 1 jobs and child sessions")
            && error.contains(&refused.to_string())
            && error.contains(CLOSE_REFUSED),
        "the failure names the child and its cause: {error}"
    );
    Ok(())
}

#[tokio::test]
async fn cancel_action_reports_an_unexpected_reply() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    fixture.script().cancel_unexpected.insert(child);
    let args = format!("{{\"action\":\"cancel\",\"ids\":[\"{child}\"]}}");
    let error = fixture
        .runtime
        .tool(
            fixture.session,
            fixture.caller(),
            CallId::new("call"),
            "agents",
            RawJson::parse(&args)?,
            CancellationToken::new(),
        )
        .await
        .err()
        .ok_or("an unexpected cancel reply must be reported")?
        .to_string();
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(
        error.contains(&child.to_string())
            && error.contains("unexpected reply to a cancel request"),
        "the unexpected reply is named: {error}"
    );
    Ok(())
}

#[tokio::test]
async fn abort_reports_active_sessions_after_the_sweep_limit() -> TestResult {
    let fixture = Fixture::open().await?;
    let ids: Vec<_> = (0..1024).map(|_| SessionId::new_v7()).collect();
    {
        let mut script = fixture.script();
        for id in &ids {
            script
                .lists
                .push_back(Ok(vec![agent(*id, AgentState::Running)]));
        }
    }
    let reply = fixture
        .runtime
        .command(fixture.session, "abort", "")
        .await?;
    let cancels = fixture.host.cancels();
    assert!(!cancels.is_empty(), "the sweep should attempt cancellation");
    assert!(
        cancels.len() < ids.len(),
        "the bounded sweep must leave an active session to report"
    );
    let remaining = ids
        .iter()
        .find(|id| !cancels.contains(id))
        .ok_or("the sweep cancelled every scripted active session")?;
    assert!(
        reply.contains(&remaining.to_string()),
        "the remaining active child is reported: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn abort_continues_when_the_turn_idle_check_fails() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.turn_idle_error = Some("the turn state is unavailable");
        script
            .lists
            .push_back(Ok(vec![agent(child, AgentState::Running)]));
    }
    let reply = fixture
        .runtime
        .command(fixture.session, "abort", "")
        .await?;
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(
        reply.contains("the turn state is unavailable"),
        "the turn failure is reported: {reply}"
    );
    Ok(())
}

#[tokio::test]
async fn abort_continues_when_the_jobs_list_fails() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.jobs_list_error = Some("the jobs list is unavailable");
        script
            .lists
            .push_back(Ok(vec![agent(child, AgentState::Running)]));
    }
    let reply = fixture
        .runtime
        .command(fixture.session, "abort", "")
        .await?;
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(
        reply.contains("the jobs list is unavailable"),
        "the jobs-list failure is reported: {reply}"
    );
    Ok(())
}

async fn wait_for_starts(fixture: &Fixture, count: usize) -> TestResult {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let changed = fixture.host.changed.notified();
            if fixture.script().starts.len() >= count {
                return;
            }
            changed.await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn runs_return_before_children_end_and_pools_bound_admission() -> TestResult {
    let fixture = Fixture::open().await?;
    let hold = CancellationToken::new();
    fixture.script().hold = Some(hold.clone());
    let reply = fixture.tool(r#"{"action":"run","steps":[{"name":"pool","prompt":"inspect {{item}}","items":["one","two","three","four"],"workers":2,"tools":["read"],"isolation":"shared"}]}"#).await?;
    assert!(reply.contains("4 subagents planned"), "{reply}");
    wait_for_starts(&fixture, 2).await?;
    assert_eq!(
        fixture.script().starts.len(),
        2,
        "the remaining items wait for a slot"
    );
    assert!(
        fixture
            .script()
            .jobs
            .iter()
            .all(|(job, _, _)| matches!(job.state, JobStateView::Running))
    );
    hold.cancel();
    let report = fixture.ended_run().await?;
    assert_eq!(fixture.script().starts.len(), 4);
    assert!(report.contains("done"), "{report}");
    assert_eq!(
        fixture
            .script()
            .jobs
            .iter()
            .filter(|(_, parent, _)| parent.is_none())
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn independent_steps_start_together_and_forward_dependencies_wait() -> TestResult {
    let fixture = Fixture::open().await?;
    let hold = CancellationToken::new();
    fixture.script().hold = Some(hold.clone());
    fixture.script().report = Some("source report".into());
    fixture.tool(r#"{"action":"run","steps":[{"name":"summary","prompt":"summarize {{step:source}}","after":["source"],"tools":["read"],"isolation":"shared"},{"name":"source","prompt":"source","tools":["read"],"isolation":"shared"},{"name":"independent","prompt":"independent","tools":["read"],"isolation":"shared"}]}"#).await?;
    wait_for_starts(&fixture, 2).await?;
    {
        let script = fixture.script();
        assert_eq!(script.starts.len(), 2);
        assert!(
            script
                .starts
                .iter()
                .all(|(_, start)| !start.prompt.contains("summarize"))
        );
    }
    hold.cancel();
    let report = fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 3);
    assert!(
        script.starts[2]
            .1
            .prompt
            .contains("summarize source report"),
        "{:?}",
        script.starts[2].1.prompt
    );
    assert!(
        report.find("summary").ok_or("summary missing")?
            < report.find("source").ok_or("source missing")?
    );
    Ok(())
}

#[tokio::test]
async fn blocked_reports_remain_blocked_and_unresolved_item_sources_skip() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().report_status = Some("blocked");
    fixture.script().report = Some("need access".into());
    fixture.tool(r#"{"action":"run","steps":[{"name":"source","prompt":"inspect","tools":["read"],"isolation":"shared"},{"name":"pool","prompt":"inspect {{item}}","items_from":"source","after":["source"],"tools":["read"],"isolation":"shared"}]}"#).await?;
    let report = fixture.ended_run().await?;
    assert_eq!(
        fixture.script().starts.len(),
        1,
        "a blocked source cannot make pool items"
    );
    assert!(
        report.contains("blocked") && report.contains("skipped"),
        "{report}"
    );
    assert!(report.contains("need access"), "{report}");
    Ok(())
}

#[tokio::test]
async fn admission_refuses_another_run_without_starting_jobs_or_children() -> TestResult {
    let mut config = parse_config(None)?;
    config.agents.max_runs = 1;
    let fixture = Fixture::open_config(config).await?;
    let hold = CancellationToken::new();
    fixture.script().hold = Some(hold.clone());
    let args = r#"{"action":"run","steps":[{"name":"task","prompt":"inspect","tools":["read"],"isolation":"shared"}]}"#;
    fixture.tool(args).await?;
    wait_for_starts(&fixture, 1).await?;
    let error = fixture
        .tool(args)
        .await
        .err()
        .ok_or("another live run must be refused")?;
    assert!(error.to_string().contains("run"), "{error}");
    assert_eq!(fixture.script().starts.len(), 1);
    assert_eq!(fixture.script().jobs.len(), 2);
    hold.cancel();
    fixture.ended_run().await?;
    Ok(())
}

#[tokio::test]
async fn worktree_preflight_refuses_missing_root_before_jobs_start() -> TestResult {
    let fixture = Fixture::open().await?;
    let error = fixture.tool(r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#)
        .await.err().ok_or("worktree run needs the host data root")?;
    assert!(error.to_string().contains("data_root"), "{error}");
    assert!(fixture.script().jobs.is_empty());
    assert!(fixture.script().starts.is_empty());
    Ok(())
}

fn output(text: &str) -> RunOutput {
    RunOutput {
        status: dal_core::ExitStatusKind::Exited(0),
        stdout_prefix: text.as_bytes().to_vec(),
        stdout_tail: text.as_bytes().to_vec(),
        stdout_prefix_overflowed: false,
        stderr_tail: Vec::new(),
        log: None,
    }
}

fn isolation_config()
-> Result<crate::orchestration::OrchestrationConfig, Box<dyn std::error::Error>> {
    let mut config = parse_config(None)?;
    config.data_root = Some(std::env::temp_dir().join("orchestration-script-data"));
    Ok(config)
}

#[tokio::test]
async fn failed_stash_preflight_cannot_silently_drop_tracked_changes() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output("/tmp")),
        Ok(output("head")),
        Err("stash could not read tracked files"),
    ]);
    let error = fixture.tool(r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#)
        .await.err().ok_or("stash failure must refuse the run")?;
    assert!(
        error
            .to_string()
            .contains("stash could not read tracked files"),
        "{error}"
    );
    assert!(fixture.script().jobs.is_empty());
    assert!(fixture.script().starts.is_empty());
    Ok(())
}

#[tokio::test]
async fn isolated_tasks_save_patch_and_merge_from_the_parent_workspace() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output("/tmp")),
        Ok(output("head")),
        Ok(output("snapshot")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("diff --git a/code b/code\n")),
        Ok(output("code\n")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
    ]);
    fixture.tool(r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#).await?;
    let report = fixture.ended_run().await?;
    assert!(report.contains("merged"), "{report}");
    let script = fixture.script();
    let task = script
        .jobs
        .iter()
        .find(|(_, parent, _)| parent.is_some())
        .ok_or("task absent")?
        .0
        .id;
    assert!(
        script
            .artifacts
            .iter()
            .any(|(job, file, bytes)| *job == task
                && *file == ArtifactFile::DeltaPatch
                && bytes.starts_with(b"diff"))
    );
    assert!(
        script
            .artifacts
            .iter()
            .any(|(job, file, _)| *job == task && *file == ArtifactFile::SummaryTxt)
    );
    assert!(
        script
            .run_requests
            .iter()
            .all(|request| request.argv.first().is_some_and(|argv| argv == "git"))
    );
    assert!(
        script
            .run_requests
            .iter()
            .filter(|request| request.argv.iter().any(|argv| argv == "apply"))
            .all(|request| request.cwd.as_deref() == Some(std::env::temp_dir().as_path()))
    );
    Ok(())
}

#[tokio::test]
async fn waiting_for_a_run_does_not_block_cancel_and_all_tasks_settle() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().hold = Some(CancellationToken::new());
    fixture.tool(r#"{"action":"run","steps":[{"name":"pool","prompt":"inspect {{item}}","items":["one","two","three"],"workers":1,"tools":["read"],"isolation":"shared"}]}"#).await?;
    wait_for_starts(&fixture, 1).await?;
    let run = fixture
        .script()
        .jobs
        .iter()
        .find(|(_, parent, _)| parent.is_none())
        .ok_or("run absent")?
        .0
        .id;
    let wait_args = format!(r#"{{"action":"wait","ids":["{run}"],"timeout":5}}"#);
    let cancel_args = format!(r#"{{"action":"cancel","ids":["{run}"]}}"#);
    let (waited, cancelled) = tokio::join!(fixture.tool(&wait_args), async {
        tokio::task::yield_now().await;
        fixture.tool(&cancel_args).await
    },);
    cancelled?;
    let waited = waited?;
    assert!(waited.contains("cancelled"), "{waited}");
    assert!(
        waited.contains("3 cancelled"),
        "the owner settles the complete summary: {waited}"
    );
    let script = fixture.script();
    assert_eq!(script.jobs.len(), 4);
    assert!(
        script
            .jobs
            .iter()
            .all(|(job, _, _)| matches!(job.state, JobStateView::Done(_)))
    );
    Ok(())
}

#[tokio::test]
async fn user_cancel_pauses_automatic_work_but_guard_pause_reason_wins() -> TestResult {
    use dal_agent::ext::{Hook as _, ObserveHook as _};
    use dal_core::ext::{ToolCallEvent, TurnEnd};
    let fixture = Fixture::open().await?;
    let turn = dal_core::TurnId::new(std::num::NonZeroU64::MIN);
    let hook = super::ToolCallHook(fixture.runtime.clone());
    for index in 0..10 {
        hook.call(
            ToolCallEvent {
                turn,
                call: CallId::new(format!("read-{index}")),
                tool: dal_core::Name::parse("read")?,
                class: dal_core::ToolClass::Read,
                args: RawJson::parse(r#"{"path":"a"}"#)?,
            },
            HookCx::for_test(fixture.host.clone(), fixture.session, Some(turn)),
        )
        .await?;
    }
    super::TurnEndHook(fixture.runtime.clone())
        .call(
            TurnEnd {
                turn,
                stop: Stop::Cancelled,
            },
            HookCx::for_test(fixture.host.clone(), fixture.session, Some(turn)),
        )
        .await?;
    let status = fixture
        .runtime
        .snapshot(fixture.session)
        .ok_or("status absent")?
        .text
        .ok_or("status text absent")?;
    assert!(status.ends_with(" · paused"), "{status}");
    assert!(!status.contains(['{', '}', '"']), "{status}");
    // The next cancellation has no guard cause.
    super::TurnEndHook(fixture.runtime.clone())
        .call(
            TurnEnd {
                turn,
                stop: Stop::Cancelled,
            },
            HookCx::for_test(fixture.host.clone(), fixture.session, None),
        )
        .await?;
    let status = fixture
        .runtime
        .snapshot(fixture.session)
        .ok_or("status absent")?
        .text
        .ok_or("status text absent")?;
    assert!(status.ends_with(" · paused"), "{status}");
    Ok(())
}

#[tokio::test]
async fn status_reports_working_after_before_turn() -> TestResult {
    use dal_agent::ext::Hook as _;
    use dal_core::ext::BeforeTurn;
    let fixture = Fixture::open().await?;
    let turn = dal_core::TurnId::new(std::num::NonZeroU64::MIN);
    super::BeforeTurnHook(fixture.runtime.clone())
        .call(
            BeforeTurn {
                turn,
                text: "work".into(),
            },
            HookCx::for_test(fixture.host.clone(), fixture.session, Some(turn)),
        )
        .await?;
    let status = fixture
        .runtime
        .snapshot(fixture.session)
        .ok_or("status absent")?
        .text
        .ok_or("status text absent")?;
    assert_eq!(status.as_ref(), "working");
    Ok(())
}

#[tokio::test]
async fn monitor_lines_reach_a_wake_and_old_live_jobs_show_silence() -> TestResult {
    let fixture = Fixture::open().await?;
    let job = JobId::new_v7();
    {
        let mut script = fixture.script();
        script.jobs.push((
            JobStatus {
                id: job,
                label: "server".into(),
                state: JobStateView::Running,
                log: None,
                last_activity_at: dal_core::Timestamp::from_second(0)?,
            },
            None,
            String::new(),
        ));
        script.lines.push_back(dal_core::JobLines {
            lines: vec![dal_core::JobLine {
                seq: 1,
                text: "server READY".into(),
            }],
            next: 1,
            dropped: 0,
            ended: false,
        });
    }
    let args = format!(r#"{{"action":"watch","job":"{job}","filter":"READY"}}"#);
    fixture
        .runtime
        .tool(
            fixture.session,
            fixture.caller(),
            CallId::new("monitor"),
            "monitor",
            RawJson::parse(&args)?,
            CancellationToken::new(),
        )
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let changed = fixture.host.changed.notified();
            if fixture
                .script()
                .wakes
                .iter()
                .any(|text| text.contains("server READY"))
            {
                break;
            }
            changed.await;
        }
    })
    .await?;
    assert!(
        !fixture.script().line_reads.is_empty(),
        "the monitor subscribes to job lines"
    );
    assert_eq!(
        fixture.script().wakes[0].matches("server READY").count(),
        1,
        "the line cursor prevents replaying the same output"
    );
    let listing = fixture.tool(r#"{"action":"list"}"#).await?;
    assert!(listing.contains("silent"), "{listing}");
    Ok(())
}

#[tokio::test]
async fn settled_tasks_are_delivered_only_in_the_single_top_level_run_report() -> TestResult {
    let fixture = Fixture::open().await?;
    run_one_step(&fixture).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let changed = fixture.host.changed.notified();
            if !fixture.script().delivered.is_empty() {
                break;
            }
            changed.await;
        }
    })
    .await?;
    let script = fixture.script();
    assert_eq!(
        script.delivered.len(),
        1,
        "task rows are not independent reports"
    );
    assert_eq!(script.wakes.len(), 1);
    assert!(
        script.wakes[0].contains("scan finished"),
        "{}",
        script.wakes[0]
    );
    Ok(())
}

#[tokio::test]
async fn saved_workflow_input_reaches_the_started_child_prompt() -> TestResult {
    let mut config = parse_config(None)?;
    config.workflows = Some(toml::from_str(
        r#"
        [inspect]
        steps = [{name = "scan", prompt = "inspect {{input}}", tools = ["read"], isolation = "shared"}]
    "#,
    )?);
    let fixture = Fixture::open_config(config).await?;
    fixture
        .tool(r#"{"action":"run","workflow":"inspect","input":"the parser"}"#)
        .await?;
    fixture.ended_run().await?;
    assert!(
        fixture.script().starts[0]
            .1
            .prompt
            .contains("inspect the parser")
    );
    Ok(())
}

#[tokio::test]
async fn dynamic_pools_refuse_before_starting_more_children_than_the_session_budget() -> TestResult
{
    let fixture = Fixture::open().await?;
    let steps = sonic_rs::json!([
        {"name":"first","prompt":"{{item}}","items":vec!["x";500],"workers":1,"tools":["read"],"isolation":"shared"},
        {"name":"second","prompt":"{{item}}","items":vec!["x";500],"workers":1,"tools":["read"],"isolation":"shared"},
        {"name":"third","prompt":"{{item}}","items":vec!["x";23],"workers":1,"tools":["read"],"isolation":"shared"},
        {"name":"source","prompt":"work","tools":["read"],"isolation":"shared"},
        {"name":"dynamic","prompt":"{{item}}","items_from":"source","after":["source"],"tools":["read"],"isolation":"shared"}
    ]);
    let args = sonic_rs::to_string(&sonic_rs::json!({"action":"run","steps":steps}))?;
    fixture.tool(&args).await?;
    let report = fixture.ended_run().await?;
    assert!(
        report.contains("needs 1 subagents, but the session has 0 left"),
        "{report}"
    );
    assert!(fixture.script().starts.len() <= 1024);
    assert!(
        fixture
            .script()
            .starts
            .iter()
            .all(|(_, child)| !child.name.starts_with("dynamic"))
    );
    Ok(())
}

#[tokio::test]
async fn monitor_batches_that_do_not_fit_are_delivered_in_the_next_wake() -> TestResult {
    let mut config = parse_config(None)?;
    config.monitor.coalesce_ms = 1;
    config.monitor.rate_limit_ms = 1;
    config.monitor.max_chars = 16_384;
    let fixture = Fixture::open_config(config).await?;
    let job = JobId::new_v7();
    fixture.script().jobs.push((
        JobStatus {
            id: job,
            label: "server".into(),
            state: JobStateView::Running,
            log: None,
            last_activity_at: dal_core::Timestamp::now(),
        },
        None,
        String::new(),
    ));
    for index in 1..=6 {
        let args = format!(
            r#"{{"action":"watch","job":"{job}","filter":"READY","description":"watch {index}"}}"#
        );
        fixture
            .runtime
            .tool(
                fixture.session,
                fixture.caller(),
                CallId::new(format!("monitor-{index}")),
                "monitor",
                RawJson::parse(&args)?,
                CancellationToken::new(),
            )
            .await?;
    }
    fixture.script().lines.push_back(dal_core::JobLines {
        lines: vec![dal_core::JobLine {
            seq: 1,
            text: format!("READY {}", "한".repeat(2100)).into(),
        }],
        next: 1,
        dropped: 0,
        ended: false,
    });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let changed = fixture.host.changed.notified();
            let text = fixture.script().wakes.join("\n");
            if (1..=6).all(|index| text.contains(&format!("Monitor m{index}"))) {
                break;
            }
            changed.await;
        }
    })
    .await?;
    let wakes = fixture.script().wakes.clone();
    assert!(wakes.len() >= 2, "all six batches cannot fit in one wake");
    assert_eq!(wakes.join("\n").matches("Monitor m1").count(), 1);
    Ok(())
}

#[tokio::test]
async fn session_end_drains_cancelled_run_tasks_before_the_owner_closes() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().hold = Some(CancellationToken::new());
    fixture.tool(r#"{"action":"run","steps":[{"name":"pool","prompt":"{{item}}","items":["one","two"],"workers":1,"tools":["read"],"isolation":"shared"}]}"#).await?;
    wait_for_starts(&fixture, 1).await?;
    fixture
        .runtime
        .close(
            fixture.session,
            SessionEnd {
                session: fixture.session,
                reason: "close".into(),
            },
        )
        .await;
    let script = fixture.script();
    assert_eq!(script.jobs.len(), 3);
    assert!(
        script
            .jobs
            .iter()
            .all(|(job, _, _)| matches!(job.state, JobStateView::Done(_)))
    );
    assert!(
        script
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_none())
            .ok_or("run missing")?
            .2
            .contains("2 cancelled")
    );
    Ok(())
}

#[tokio::test]
async fn failed_wait_keeps_an_unexpected_close_reply_visible() -> TestResult {
    let fixture = Fixture::open().await?;
    let child = SessionId::new_v7();
    {
        let mut script = fixture.script();
        script.started = Some(child);
        script.await_error = Some("the provider stopped answering");
        script.cancel_unexpected.insert(child);
    }
    let report = run_one_step(&fixture).await?;
    assert!(
        report.contains("the provider stopped answering"),
        "{report}"
    );
    assert!(report.contains("unexpected reply"), "{report}");
    assert_eq!(fixture.host.cancels(), vec![child]);
    Ok(())
}

#[tokio::test]
async fn an_oversized_dynamic_pool_fails_the_run_without_starting_pool_children() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().report =
        Some("item\n".repeat(crate::orchestration::pool::ITEM_LINES_LIMIT + 1));
    fixture.tool(r#"{"action":"run","steps":[{"name":"source","prompt":"list items","tools":["read"],"isolation":"shared"},{"name":"pool","prompt":"{{item}}","items_from":"source","after":["source"],"tools":["read"],"isolation":"shared"}]}"#).await?;
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 1);
    assert!(
        matches!(
            script
                .jobs
                .iter()
                .find(|(_, parent, _)| parent.is_none())
                .ok_or("run missing")?
                .0
                .state,
            JobStateView::Done(dal_core::JobOutcome::Failed { .. })
        ),
        "an oversized pool is a failure, not a successful run with a skip"
    );
    Ok(())
}

#[tokio::test]
async fn paused_and_stopped_controllers_keep_reports_queued_until_resumed() -> TestResult {
    use dal_agent::ext::ObserveHook as _;
    let mut premature = Vec::new();
    for mode in ["user cancel", "stop"] {
        let fixture = Fixture::open().await?;
        fixture.user_input().await?;
        if mode == "stop" {
            fixture
                .runtime
                .command(fixture.session, "continuation", "stop")
                .await?;
        } else {
            super::TurnEndHook(fixture.runtime.clone())
                .call(
                    dal_core::ext::TurnEnd {
                        turn: dal_core::TurnId::new(std::num::NonZeroU64::MIN),
                        stop: Stop::Cancelled,
                    },
                    HookCx::for_test(fixture.host.clone(), fixture.session, None),
                )
                .await?;
        }
        let job = JobId::new_v7();
        fixture.script().jobs.push((
            JobStatus {
                id: job,
                label: "queued report".into(),
                state: JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
                log: None,
                last_activity_at: dal_core::Timestamp::now(),
            },
            None,
            "waiting report".into(),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if !fixture.script().wakes.is_empty() {
            premature.push(mode);
        }
        if mode == "stop" {
            fixture.user_input().await?;
            let status = fixture
                .runtime
                .snapshot(fixture.session)
                .ok_or("status absent")?
                .text
                .ok_or("status text absent")?;
            assert!(status.ends_with(" · stopped"), "{status}");
            fixture
                .runtime
                .command(fixture.session, "continuation", "run")
                .await?;
        } else {
            fixture.user_input().await?;
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let changed = fixture.host.changed.notified();
                if fixture.script().delivered.contains(&job) {
                    break;
                }
                changed.await;
            }
        })
        .await?;
        assert!(
            fixture
                .script()
                .wakes
                .iter()
                .any(|text| text.contains("waiting report"))
        );
    }
    assert!(
        premature.is_empty(),
        "controllers woke before resume: {premature:?}"
    );
    Ok(())
}

fn goal_turn() -> dal_core::TurnId {
    dal_core::TurnId::new(std::num::NonZeroU64::MIN)
}

const GOAL_PROMPT_HEAD: &str = "Continue working toward the active goal.";

#[tokio::test(start_paused = true)]
async fn user_grace_fires_at_ten_seconds_and_a_prompt_drops_it() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture
        .user_turn(turn, Stop::EndTurn, "made progress")
        .await?;
    fixture.pump().await;
    assert!(
        fixture.script().wakes.is_empty(),
        "the grace scheduled no wake yet"
    );
    tokio::time::advance(std::time::Duration::from_millis(9_900)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert!(
            script.wakes.is_empty(),
            "no continuation before ten seconds: {:?}",
            script.wakes
        );
    }
    // A prompt inside the window drops the scheduled continuation.
    fixture.user_input().await?;
    tokio::time::advance(std::time::Duration::from_millis(1_000)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert!(
            script.wakes.is_empty(),
            "the prompt dropped the scheduled continuation: {:?}",
            script.wakes
        );
    }
    // The dropped prompt's own turn schedules a fresh grace, which fires.
    fixture.begin_turn(turn).await?;
    fixture
        .end_turn(turn, Stop::EndTurn, "changed direction")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert_eq!(script.wakes.len(), 1, "{:?}", script.wakes);
        assert!(
            script.wakes[0].starts_with(GOAL_PROMPT_HEAD),
            "{}",
            script.wakes[0]
        );
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn idle_wake_joins_the_goal_continuation_to_job_reports() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    // No continuation is scheduled, so the wake for the job report
    // evaluates the goal verdict on the Idle path.
    fixture.done_job("server exited: 2 problems left");
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert_eq!(script.wakes.len(), 1, "{:?}", script.wakes);
        let report = &script.wakes[0];
        let Some(goal_at) = report.find(GOAL_PROMPT_HEAD) else {
            return Err(format!("the wake carries no goal prompt: {report}").into());
        };
        let Some(jobs_at) = report.find("server exited: 2 problems left") else {
            return Err(format!("the wake carries no job report: {report}").into());
        };
        assert!(jobs_at < goal_at, "P2 precedes P4: {report}");
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn provider_error_blocks_and_the_next_prompt_recovers() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture.user_turn(turn, Stop::Failed, "").await?;
    tokio::time::advance(std::time::Duration::from_secs(12)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert!(
            script.wakes.is_empty(),
            "a failed turn schedules no continuation: {:?}",
            script.wakes
        );
    }
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(
        shown.contains("blocked: provider error ended the turn (retries exhausted)"),
        "{shown}"
    );
    // The next prompt reactivates the goal; the Recovery verdict runs at
    // that prompt's turn end.
    fixture.user_turn(turn, Stop::EndTurn, "back on it").await?;
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(shown.contains("g1: active"), "{shown}");
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1, "{:?}", script.wakes);
    assert!(
        script.wakes[0].starts_with(GOAL_PROMPT_HEAD),
        "{}",
        script.wakes[0]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn automatic_turn_continuation_is_ready_at_once() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture
        .user_turn(turn, Stop::EndTurn, "made progress")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    assert_eq!(fixture.script().wakes.len(), 1);
    // The wake turn was automatic: its opening runs before_turn, which
    // clears the prompt latch, and its continuation waits no grace.
    fixture.begin_turn(turn).await?;
    fixture
        .end_turn(turn, Stop::EndTurn, "more progress")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    {
        let script = fixture.script();
        assert_eq!(script.wakes.len(), 2, "{:?}", script.wakes);
    }
    Ok(())
}
