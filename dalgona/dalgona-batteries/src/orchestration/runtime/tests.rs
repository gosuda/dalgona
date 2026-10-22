// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Cancellation and settlement of child sessions, driven through the real
//! session owner against a scripted host.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicUsize;
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
    JobsOp, JobsReply, ModelRequest, Notice, PrivateEffect, Question, RawJson, RunOutput,
    RunRequest, SessionId, SidecarOp, StateError, StateOp, StateRecord, Stop, TurnOp, TurnOpReply,
};
use serde::Deserialize;
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
    /// Job lists answered normally before `jobs_list_error` applies.
    jobs_list_skip: usize,
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
    /// Wake and sidecar calls in service order.
    events: Vec<&'static str>,
    taken: HashSet<JobId>,
    delivered: HashSet<JobId>,
    lines: VecDeque<dal_core::JobLines>,
    line_reads: Vec<Option<u64>>,
    /// A failure the host answers the next job-lines read with.
    lines_error: Option<&'static str>,
    /// A failure the host answers the next sidecar write with.
    sidecar_write_error: Option<&'static str>,
    /// A failure the host answers the next sidecar artifact write with.
    artifact_fail: Option<&'static str>,
    /// A failure the host answers the next write of one named artifact file
    /// with; other artifact files persist normally.
    artifact_fail_file: Option<(ArtifactFile, &'static str)>,
    /// Persisted artifacts and worktree removals in service order, so a test
    /// can prove which record was durable before a tree was removed.
    salvage_order: Vec<&'static str>,
    /// The sidecar files the session wrote, by name.
    sidecar_files: std::collections::HashMap<String, Vec<u8>>,
    /// A refusal the host answers every start with.
    refuse_start: Option<dal_core::AgentRefusal>,
    /// Whether awaited children end without storing a report.
    silent: bool,
    /// Whether the next settled job makes the session abort its runs, the way
    /// a user abort lands between two steps.
    abort_on_settle: bool,
    /// Grace prompts the host accepted, in order.
    prompts: Vec<(SessionId, String)>,
    /// Await calls served so far; each later one reports a new entry.
    awaits: u64,
    /// Scripted answers to the next commits, oldest first; an empty queue
    /// commits every id.
    commit_replies: VecDeque<CommitReply>,
    /// The ids of every commit the host served.
    commits: Vec<Vec<JobId>>,
    /// The ids of every release the host served.
    releases: Vec<Vec<JobId>>,
    /// A refusal the host answers the next wake with.
    wake_refusal: Option<&'static str>,
    /// A wake-limit denial the host answers the next wake with, the way the
    /// core answers once a session passes its automatic-turn cutoff. The
    /// count itself stays core-owned; the battery proves only stop-on-denial.
    wake_denied_limit: bool,
    /// A failure the host answers the next release with.
    release_fail: Option<&'static str>,
    /// The host's durable held set; `Hold` and `Unhold` change it and answer
    /// with all of it, the way the real jobs service does.
    held: Vec<JobId>,
    /// A failure the host answers the next hold or unhold with.
    held_error: Option<&'static str>,
}

/// How the scripted host answers one commit.
enum CommitReply {
    /// The host fails the call; no id is committed.
    Fail(&'static str),
    /// The host commits only the first id and returns it alone.
    First,
}

/// A host that answers only the services a session owner uses when it
/// cancels children; every other service reports itself unavailable.
#[derive(Default)]
struct Host {
    script: Mutex<Script>,
    this: Weak<Host>,
    runtime: Mutex<Option<(Runtime, SessionId)>>,
    changed: tokio::sync::Notify,
    apply_gate: Option<Arc<ApplyGate>>,
    stage_gate: Option<Arc<ApplyGate>>,
    passthrough_git: bool,
    /// When set, each started child with its own workspace gets one distinct
    /// file written into that workspace, standing in for the child's own
    /// patch tool in the real-git scale test. The counter names the files.
    stress_files: Option<Arc<AtomicUsize>>,
}

fn is_plain_apply(argv: &[std::ffi::OsString]) -> bool {
    argv.iter().any(|word| word == "apply") && !argv.iter().any(|word| word == "--check")
}

fn is_add_all(argv: &[std::ffi::OsString]) -> bool {
    argv.iter().any(|word| word == "add") && argv.iter().any(|word| word == "-A")
}

/// Parks every plain `git apply` of the hosts that share it until the test
/// releases one, and logs who entered and left.
struct ApplyGate {
    log: Mutex<Vec<ApplyEvent>>,
    changed: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyEvent {
    Enter(SessionId),
    Exit(SessionId),
}

impl ApplyGate {
    fn new() -> Self {
        Self {
            log: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }

    async fn pass(&self, session: SessionId) {
        locked(&self.log).push(ApplyEvent::Enter(session));
        self.changed.notify_waiters();
        if let Ok(permit) = self.release.acquire().await {
            permit.forget();
        }
        locked(&self.log).push(ApplyEvent::Exit(session));
        self.changed.notify_waiters();
    }

    fn events(&self) -> Vec<ApplyEvent> {
        locked(&self.log).clone()
    }

    async fn entered(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            let entered = locked(&self.log)
                .iter()
                .filter(|event| matches!(event, ApplyEvent::Enter(_)))
                .count();
            if entered >= count {
                return;
            }
            changed.await;
        }
    }
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
        let stress_write = self.stress_files.as_ref().and_then(|counter| {
            start.workspace.as_ref().map(|workspace| {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    workspace.as_path().join(format!("unit-{n:04}.txt")),
                    format!("unit {n}\n").into_bytes(),
                )
            })
        });
        Box::pin(async move {
            if let Some((path, bytes)) = stress_write {
                tokio::fs::write(&path, &bytes).await.map_err(|error| {
                    ServiceError::failed(None, format!("stress file {path:?}: {error}"))
                })?;
            }
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
    fn commit_reports(script: &mut Script, mut ids: Vec<JobId>) -> Result<JobsReply, ServiceError> {
        script.commits.push(ids.clone());
        match script.commit_replies.pop_front() {
            Some(CommitReply::Fail(message)) => return Err(ServiceError::failed(None, message)),
            Some(CommitReply::First) => ids.truncate(1),
            None => {}
        }
        script.delivered.extend(ids.iter().copied());
        for id in &ids {
            script.taken.remove(id);
        }
        Ok(JobsReply::Committed { ids })
    }

    /// Adds one job to the scripted held set and answers with all of it.
    fn hold_job(script: &mut Script, id: JobId) -> Result<JobsReply, ServiceError> {
        if let Some(message) = script.held_error.take() {
            return Err(ServiceError::failed(None, message));
        }
        if !script.held.contains(&id) {
            script.held.push(id);
        }
        Ok(JobsReply::Held(script.held.clone()))
    }

    /// Removes one job from the scripted held set and answers with all of it.
    fn unhold_job(script: &mut Script, id: JobId) -> Result<JobsReply, ServiceError> {
        if let Some(message) = script.held_error.take() {
            return Err(ServiceError::failed(None, message));
        }
        script.held.retain(|held| *held != id);
        Ok(JobsReply::Held(script.held.clone()))
    }

    fn release_reports(script: &mut Script, ids: Vec<JobId>) -> Result<JobsReply, ServiceError> {
        if let Some(message) = script.release_fail.take() {
            return Err(ServiceError::failed(None, message));
        }
        script.releases.push(ids.clone());
        for id in &ids {
            script.taken.remove(id);
        }
        Ok(JobsReply::Released { ids })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "test seam runs the real git the battery under test drives"
)]
async fn real_git(req: RunRequest) -> Result<RunOutput, ServiceError> {
    let (argv, cwd, stdin, env, limit) = (
        req.argv,
        req.cwd,
        req.stdin,
        req.env,
        req.stdout_prefix_limit,
    );
    let out = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(&argv[0]);
        if argv.len() > 1 {
            command.args(&argv[1..]);
        }
        if let Some(dir) = cwd.as_ref() {
            command.current_dir(dir);
        }
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0");
        for (key, value) in &env {
            command.env(key.as_ref(), value.as_ref());
        }
        if stdin.is_some() {
            command.stdin(std::process::Stdio::piped());
        }
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| error.to_string())?;
        if let Some(input) = stdin.as_ref() {
            use std::io::Write as _;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| "git stdin was unavailable".to_string())?
                .write_all(input)
                .map_err(|error| error.to_string())?;
        }
        child.wait_with_output().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| ServiceError::failed(None, error.to_string()))?
    .map_err(|message| ServiceError::failed(None, message))?;
    let code = out.status.code().unwrap_or(1);
    let overflowed = out.stdout.len() > limit as usize;
    let mut prefix = out.stdout;
    if overflowed {
        prefix.truncate(limit as usize);
    }
    let tail = prefix.clone();
    Ok(RunOutput {
        status: dal_core::ExitStatusKind::Exited(code),
        stdout_prefix: prefix,
        stdout_prefix_overflowed: overflowed,
        stdout_tail: tail,
        stderr_tail: out.stderr,
        log: None,
    })
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
        let session = locked(&self.runtime).as_ref().map(|(_, session)| *session);
        let apply_parked = self
            .apply_gate
            .clone()
            .filter(|_| is_plain_apply(&req.argv))
            .zip(session);
        let stage_parked = self
            .stage_gate
            .clone()
            .filter(|_| is_add_all(&req.argv))
            .zip(session);
        if self.passthrough_git && req.argv.first().is_some_and(|word| word == "git") {
            return Box::pin(async move {
                if let Some((gate, session)) = stage_parked {
                    gate.pass(session).await;
                }
                if let Some((gate, session)) = apply_parked {
                    gate.pass(session).await;
                }
                real_git(req).await
            });
        }
        let mut script = locked(&self.script);
        if req.argv.iter().any(|word| word == "worktree") {
            for verb in ["remove", "prune"] {
                if req.argv.iter().any(|word| word == verb) {
                    script.salvage_order.push(verb);
                }
            }
        }
        script.run_requests.push(req);
        let reply = script.run_outputs.pop_front();
        Box::pin(async move {
            if let Some((gate, session)) = stage_parked {
                gate.pass(session).await;
            }
            if let Some((gate, session)) = apply_parked {
                gate.pass(session).await;
            }
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
                                CallId::new(crate::orchestration::agents_tool::REPORT_TOOL_NAME),
                                crate::orchestration::agents_tool::REPORT_TOOL_NAME,
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

    #[expect(
        clippy::too_many_lines,
        reason = "scripted-host JobsOp dispatch lists every operation arm explicitly so failure messages name the missing arm"
    )]
    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        if let JobsOp::Wait { id, .. } = op {
            return self.wait_job(id);
        }
        let mut script = locked(&self.script);
        let settled_task = match &op {
            JobsOp::Settle { id, .. } => script
                .jobs
                .iter()
                .any(|(job, parent, _)| job.id == *id && parent.is_some()),
            _ => false,
        };
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
            JobsOp::List => {
                let failure = if script.jobs_list_skip > 0 {
                    script.jobs_list_skip -= 1;
                    None
                } else {
                    script.jobs_list_error.take()
                };
                match failure {
                    Some(message) => Err(ServiceError::failed(None, message)),
                    None => Ok(JobsReply::Listed(
                        script.jobs.iter().map(|(job, _, _)| job.clone()).collect(),
                    )),
                }
            }
            JobsOp::Lines { after, .. } => {
                if let Some(message) = script.lines_error.take() {
                    Err(ServiceError::failed(None, message))
                } else {
                    script.line_reads.push(after);
                    let mut lines = script.lines.front().cloned().unwrap_or_default();
                    lines.lines.retain(|line| line.seq > after.unwrap_or(0));
                    Ok(JobsReply::Lines(lines))
                }
            }
            JobsOp::Take { limit } => Ok(Self::take_reports(&mut script, limit)),
            JobsOp::Commit { ids } => Self::commit_reports(&mut script, ids),
            JobsOp::Release { ids } => Self::release_reports(&mut script, ids),
            JobsOp::Hold { id } => Self::hold_job(&mut script, id),
            JobsOp::Unhold { id } => Self::unhold_job(&mut script, id),
            _ => Err(ServiceError::failed(None, "unscripted job operation")),
        };
        let aborts = settled_task && reply.is_ok() && std::mem::take(&mut script.abort_on_settle);
        drop(script);
        self.changed.notify_waiters();
        match locked(&self.runtime).clone() {
            Some((runtime, session)) if aborts => Box::pin(async move {
                let _ = runtime.command(session, "abort", "").await;
                reply
            }),
            _ => Box::pin(async move { reply }),
        }
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
                let mut script = locked(&self.script);
                script.events.push("wake");
                if script.wake_denied_limit {
                    script.wake_denied_limit = false;
                    return Box::pin(async move {
                        Err(ServiceError::Denied(dal_core::DenyReason::WakeLimit))
                    });
                }
                if let Some(message) = script.wake_refusal.take() {
                    return Box::pin(async move { Err(ServiceError::failed(None, message)) });
                }
                script.wakes.push(text.into());
                drop(script);
                self.changed.notify_waiters();
                Box::pin(async { Ok(TurnOpReply::Woken) })
            }
            _ => unavailable(),
        }
    }
    fn sidecar(&self, _who: &Caller, op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let mut script = locked(&self.script);
        let reply = match op {
            SidecarOp::Artifact { job, file, bytes } => {
                if let Some((failing, message)) = script.artifact_fail_file
                    && failing == file
                {
                    script.artifact_fail_file = None;
                    drop(script);
                    return Box::pin(async move { Err(ServiceError::failed(None, message)) });
                }
                if let Some(message) = script.artifact_fail.take() {
                    drop(script);
                    return Box::pin(async move { Err(ServiceError::failed(None, message)) });
                }
                script.salvage_order.push(match file {
                    ArtifactFile::DeltaPatch => "artifact:delta_patch",
                    ArtifactFile::StatusTxt => "artifact:status_txt",
                    _ => "artifact:other",
                });
                script.artifacts.push((job, file, bytes));
                None
            }
            SidecarOp::Write { name, bytes } => {
                script.events.push("sidecar");
                if let Some(message) = script.sidecar_write_error.take() {
                    drop(script);
                    return Box::pin(async move { Err(ServiceError::failed(None, message)) });
                }
                script.sidecar_files.insert(name.to_string(), bytes);
                None
            }
            SidecarOp::Read { name } => script.sidecar_files.get(&name.to_string()).cloned(),
            _ => None,
        };
        Box::pin(async move { Ok(reply) })
    }

    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        unavailable()
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

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
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
        Self::open_on(Runtime::new(config)?, None).await
    }

    /// Opens one more session of `runtime`; sessions that share a runtime
    /// share its workspace merge locks.
    async fn open_on(
        runtime: Runtime,
        apply_gate: Option<Arc<ApplyGate>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let host = Arc::new_cyclic(|this| Host {
            this: this.clone(),
            apply_gate,
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
    /// Opens one more session of `runtime` at `workspace`; sessions that
    /// share a runtime share its checkout merge locks.
    async fn open_at(
        runtime: Runtime,
        apply_gate: Option<Arc<ApplyGate>>,
        workspace: dal_core::Workspace,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let host = Arc::new_cyclic(|this| Host {
            this: this.clone(),
            apply_gate,
            ..Host::default()
        });
        let session = SessionId::new_v7();
        *locked(&host.runtime) = Some((runtime.clone(), session));
        let services: Arc<dyn Services> = host.clone();
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
                    model: None,
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
        self.end_turn_with(turn, stop, false, reply_text).await
    }

    /// Ends the active turn as `end_turn` does, stating whether the core
    /// reported a context overflow.
    async fn end_turn_with(
        &self,
        turn: dal_core::TurnId,
        stop: Stop,
        overflowed: bool,
        reply_text: &str,
    ) -> TestResult {
        use dal_agent::ext::ObserveHook as _;
        super::TurnEndHook(self.runtime.clone())
            .call(
                dal_core::ext::TurnEnd {
                    turn,
                    stop,
                    overflowed,
                },
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
    assert_eq!(fixture.host.prompts().len(), 0);
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
    assert_eq!(fixture.host.notices().len(), 0);
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
    // A paused controller keeps the owner's delivery tick from calling the
    // host, so the one-shot error below is consumed by the abort alone.
    fixture
        .runtime
        .command(fixture.session, "continuation", "pause")
        .await?;
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
    // A paused controller keeps the owner's delivery tick from calling the
    // host, so the one-shot error below is consumed by the abort alone.
    fixture
        .runtime
        .command(fixture.session, "continuation", "pause")
        .await?;
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

#[tokio::test]
async fn list_with_ids_shows_the_requested_job_past_the_limit() -> TestResult {
    let fixture = Fixture::open().await?;
    let wanted = JobId::new_v7();
    {
        let mut script = fixture.script();
        for index in 0..super::super::agents_tool::LIST_RUNS_LIMIT {
            script.jobs.push((
                JobStatus {
                    id: JobId::new_v7(),
                    label: format!("filler-{index}").into(),
                    state: JobStateView::Running,
                    log: None,
                    last_activity_at: dal_core::Timestamp::now(),
                },
                None,
                String::new(),
            ));
        }
        script.jobs.push((
            JobStatus {
                id: wanted,
                label: "target".into(),
                state: JobStateView::Running,
                log: None,
                last_activity_at: dal_core::Timestamp::now(),
            },
            None,
            String::new(),
        ));
    }
    let unfiltered = fixture.tool(r#"{"action":"list"}"#).await?;
    assert!(
        !unfiltered.contains("target: running"),
        "the unfiltered list still stops at the limit: {unfiltered}"
    );
    let filtered = fixture
        .tool(&format!(r#"{{"action":"list","ids":["{wanted}"]}}"#))
        .await?;
    assert!(
        filtered.contains(&format!("{wanted} target: running")),
        "the requested job is shown: {filtered}"
    );
    assert!(
        !filtered.contains("filler-0"),
        "other jobs stay hidden: {filtered}"
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
    assert_eq!(fixture.script().jobs.len(), 0);
    assert_eq!(fixture.script().starts.len(), 0);
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
async fn opening_a_session_registers_no_workspace_lock() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    assert_eq!(fixture.runtime.merge_locks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn failed_stash_preflight_cannot_silently_drop_tracked_changes() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output(&temp_toplevel())),
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
    assert_eq!(fixture.script().jobs.len(), 0);
    assert_eq!(fixture.script().starts.len(), 0);
    Ok(())
}

#[tokio::test]
async fn implicit_and_explicit_worktree_steps_start_in_distinct_workspaces() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    {
        let mut script = fixture.script();
        script.run_outputs.extend([
            Ok(output("git version 2.47.0")),
            Ok(output(&temp_toplevel())),
            Ok(output("head")),
            Ok(output("snapshot")),
        ]);
        script.run_outputs.extend((0..24).map(|_| Ok(output(""))));
    }
    fixture
        .tool(
            r#"{"action":"run","steps":[{"name":"default","prompt":"edit {{item}}","items":["one","two"],"workers":2,"tools":["patch"]},{"name":"explicit","prompt":"edit","after":["default"],"tools":["exec"],"isolation":"worktree"}]}"#,
        )
        .await?;
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 3);
    let root = std::env::temp_dir()
        .join("orchestration-script-data")
        .join("worktrees")
        .join(fixture.session.to_string());
    let mut workspaces = HashSet::new();
    for (_, start) in &script.starts {
        let workspace = start
            .workspace
            .as_ref()
            .ok_or("write-capable child inherited the shared checkout")?;
        assert!(
            workspace.as_path().starts_with(&root),
            "unexpected workspace: {}",
            workspace.as_path().display()
        );
        assert!(
            workspaces.insert(workspace.as_path().to_path_buf()),
            "two write-capable children share one worktree"
        );
    }
    let added = script
        .run_requests
        .iter()
        .filter(|request| {
            request.argv.iter().any(|word| word == "worktree")
                && request.argv.iter().any(|word| word == "add")
        })
        .map(|request| request.argv.get(6).cloned().map(std::path::PathBuf::from))
        .collect::<Option<HashSet<_>>>()
        .ok_or("malformed git worktree add call")?;
    assert_eq!(
        workspaces, added,
        "child workspaces must match created worktrees"
    );
    Ok(())
}

#[tokio::test]
async fn isolated_tasks_save_patch_and_merge_from_the_parent_workspace() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output(&temp_toplevel())),
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
                overflowed: false,
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
                overflowed: false,
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
async fn status_publish_projects_counts_goal_and_payload_to_the_view() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.pump().await;
    let view = fixture.runtime.view();
    let counts = view
        .inflight(fixture.session)
        .ok_or("the owner projects counts")?;
    assert!(!counts.is_live());
    assert_eq!(view.goal(fixture.session), None);
    let status = view
        .status(fixture.session)
        .ok_or("the owner projects status")?;
    assert!(status.contains("\"inflight\""), "{status}");
    assert!(status.contains("\"silent_jobs\":0"), "{status}");
    fixture.create_goal("ship the release").await?;
    fixture.pump().await;
    let goal = view
        .goal(fixture.session)
        .ok_or("the created goal is projected")?;
    assert_eq!(goal.id, "g1");
    assert_eq!(goal.objective, "ship the release");
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
async fn a_failing_monitor_read_neither_stops_later_jobs_nor_stays_silent() -> TestResult {
    let fixture = Fixture::open().await?;
    let (broken, healthy) = (JobId::new_v7(), JobId::new_v7());
    {
        let mut script = fixture.script();
        for (id, label) in [(broken, "broken"), (healthy, "healthy")] {
            script.jobs.push((
                JobStatus {
                    id,
                    label: label.into(),
                    state: JobStateView::Running,
                    log: None,
                    last_activity_at: dal_core::Timestamp::now(),
                },
                None,
                String::new(),
            ));
        }
        script.lines.push_back(dal_core::JobLines {
            lines: vec![dal_core::JobLine {
                seq: 1,
                text: "healthy READY".into(),
            }],
            next: 1,
            dropped: 0,
            ended: false,
        });
        script.lines_error = Some("the lines service is down");
    }
    for (job, filter) in [(broken, "BROKEN"), (healthy, "READY")] {
        let args = format!(r#"{{"action":"watch","job":"{job}","filter":"{filter}"}}"#);
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
    }
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let changed = fixture.host.changed.notified();
            if fixture
                .script()
                .wakes
                .iter()
                .any(|text| text.contains("healthy READY"))
            {
                break;
            }
            changed.await;
        }
    })
    .await?;
    assert!(
        !fixture.script().line_reads.is_empty(),
        "a job after the failing one is still polled"
    );
    let monitor_notices = fixture
        .script()
        .notices
        .iter()
        .filter(|notice| notice.kind.as_ref() == "orchestration.monitor")
        .count();
    assert_eq!(monitor_notices, 1, "the poll failure is reported once");
    assert!(
        fixture
            .script()
            .notices
            .iter()
            .any(|notice| notice.text.contains("the lines service is down")),
        "the notice names the cause"
    );
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

/// The starts one step's children received, in start order. A task's child
/// is named `<step> <n>` and a pool member `<step> <n>: <item>`.
fn step_starts(script: &Script, step: &str) -> Vec<AgentStart> {
    let prefix = format!("{step} ");
    script
        .starts
        .iter()
        .map(|(_, start)| start)
        .filter(|start| {
            start
                .name
                .as_deref()
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .cloned()
        .collect()
}

/// Asserts that one step started exactly `count` children and that every
/// one of them was handed exactly this role and this system prompt.
fn assert_child_input(
    script: &Script,
    step: &str,
    count: usize,
    role: Option<&str>,
    system: Option<&str>,
) {
    let starts = step_starts(script, step);
    assert_eq!(starts.len(), count, "children started for step {step}");
    for start in &starts {
        assert_eq!(start.role.as_deref(), role, "role of a {step} child");
        assert_eq!(start.system.as_deref(), system, "system of a {step} child");
    }
}

#[tokio::test]
async fn a_step_role_reaches_the_started_child() -> TestResult {
    let fixture = Fixture::open().await?;
    let reply = fixture
        .tool(
            r#"{"action":"run","steps":[{"name":"review","prompt":"work","role":"reviewer","isolation":"shared"}]}"#,
        )
        .await?;
    assert!(reply.starts_with("started run "), "{reply}");
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 1, "the one step starts one child");
    assert_child_input(&script, "review", 1, Some("reviewer"), None);
    Ok(())
}

#[tokio::test]
async fn a_step_system_prompt_reaches_the_started_child() -> TestResult {
    let fixture = Fixture::open().await?;
    let reply = fixture
        .tool(
            r#"{"action":"run","steps":[{"name":"audit","prompt":"work","system":"Be concise.","isolation":"shared"}]}"#,
        )
        .await?;
    assert!(reply.starts_with("started run "), "{reply}");
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 1, "the one step starts one child");
    assert_child_input(&script, "audit", 1, None, Some("Be concise."));
    Ok(())
}

#[tokio::test]
async fn each_step_hands_its_own_role_or_system_to_every_child_it_starts() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture
        .tool(
            r#"{"action":"run","steps":[
                {"name":"review","prompt":"work","role":"reviewer","isolation":"shared"},
                {"name":"audit","prompt":"work","system":"Be concise.","isolation":"shared"},
                {"name":"plain","prompt":"work","isolation":"shared"},
                {"name":"check","prompt":"check {{item}}","items":["one","two"],"role":"tester","isolation":"shared"}
            ]}"#,
        )
        .await?;
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(
        script.starts.len(),
        5,
        "one child per task step and one per pool item"
    );
    assert_child_input(&script, "review", 1, Some("reviewer"), None);
    assert_child_input(&script, "audit", 1, None, Some("Be concise."));
    assert_child_input(&script, "plain", 1, None, None);
    assert_child_input(&script, "check", 2, Some("tester"), None);
    Ok(())
}

#[tokio::test]
async fn a_step_with_role_and_system_is_refused_before_any_job_or_child_starts() -> TestResult {
    let fixture = Fixture::open().await?;
    let error = fixture
        .tool(
            r#"{"action":"run","steps":[
                {"name":"first","prompt":"work","role":"reviewer","isolation":"shared"},
                {"name":"second","prompt":"work","role":"reviewer","system":"Be concise.","isolation":"shared"}
            ]}"#,
        )
        .await
        .err()
        .ok_or("a step with both role and system must be refused")?;
    assert!(
        error
            .to_string()
            .contains("agents: step second: role and system cannot both be set."),
        "{error}"
    );
    {
        let script = fixture.script();
        assert_eq!(script.jobs.len(), 0, "a refused call spawns no run job");
        assert_eq!(
            script.starts.len(),
            0,
            "the valid first step starts no child either"
        );
    }
    // The same fixture still starts a step that sets only one of the two, so
    // the refusal above is the conflict and not an unusable host.
    let reply = fixture
        .tool(
            r#"{"action":"run","steps":[{"name":"first","prompt":"work","role":"reviewer","isolation":"shared"}]}"#,
        )
        .await?;
    assert!(reply.starts_with("started run "), "{reply}");
    fixture.ended_run().await?;
    assert_child_input(&fixture.script(), "first", 1, Some("reviewer"), None);
    Ok(())
}

#[tokio::test]
async fn saved_workflow_role_and_system_reach_the_started_children() -> TestResult {
    let mut config = parse_config(None)?;
    config.workflows = Some(toml::from_str(
        r#"
        [vet]
        steps = [
            {name = "review", prompt = "work", role = "reviewer", isolation = "shared"},
            {name = "audit", prompt = "work", system = "Be concise.", isolation = "shared"}
        ]

        [clash]
        steps = [{name = "both", prompt = "work", role = "reviewer", system = "Be concise.", isolation = "shared"}]
    "#,
    )?);
    let fixture = Fixture::open_config(config).await?;
    let error = fixture
        .tool(r#"{"action":"run","workflow":"clash"}"#)
        .await
        .err()
        .ok_or("a saved step with both role and system must be refused")?;
    let text = error.to_string();
    assert!(
        text.contains(r#"saved workflow "clash" is invalid"#)
            && text.contains("role and system cannot both be set"),
        "{text}"
    );
    {
        let script = fixture.script();
        assert_eq!(script.jobs.len(), 0, "a refused call spawns no run job");
        assert_eq!(script.starts.len(), 0, "a refused call starts no child");
    }
    let reply = fixture.tool(r#"{"action":"run","workflow":"vet"}"#).await?;
    assert!(reply.starts_with("started run "), "{reply}");
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 2, "one child per saved step");
    assert_child_input(&script, "review", 1, Some("reviewer"), None);
    assert_child_input(&script, "audit", 1, None, Some("Be concise."));
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
    assert!(fixture.script().starts.iter().all(|(_, child)| {
        !child
            .name
            .as_deref()
            .is_some_and(|name| name.starts_with("dynamic"))
    }));
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

#[tokio::test(start_paused = true)]
async fn paused_and_stopped_controllers_keep_reports_queued_until_resumed() -> TestResult {
    use dal_agent::ext::ObserveHook as _;
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
                        overflowed: false,
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
        for _ in 0..3 {
            tokio::time::advance(std::time::Duration::from_millis(250)).await;
            fixture.pump().await;
            assert!(
                fixture.script().wakes.is_empty(),
                "the {mode} controller woke before resume"
            );
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
        let mut ticks = 0;
        while !fixture.script().delivered.contains(&job) {
            assert!(
                ticks < 16,
                "the {mode} report was not delivered after resume"
            );
            ticks += 1;
            tokio::time::advance(std::time::Duration::from_millis(250)).await;
            fixture.pump().await;
        }
        assert!(
            fixture
                .script()
                .wakes
                .iter()
                .any(|text| text.contains("waiting report"))
        );
    }
    Ok(())
}

fn goal_turn() -> dal_core::TurnId {
    dal_core::TurnId::new(std::num::NonZeroU64::MIN)
}

const GOAL_PROMPT_HEAD: &str = "Continue working toward the active goal.";

/// The delivery counters the goal file holds.
#[derive(Debug, Eq, PartialEq)]
struct Counters {
    consecutive: u32,
    unattended: u32,
    goal_turns: u32,
    toolless_streak: u32,
}

impl Fixture {
    /// Reads the goal counters from the goal file the session last wrote.
    fn counters(&self) -> Result<Counters, Box<dyn std::error::Error>> {
        let bytes = self
            .script()
            .sidecar_files
            .get("goal.json")
            .cloned()
            .ok_or("the session wrote no goal file")?;
        let sidecar =
            crate::orchestration::goal::sidecar::decode_sidecar(&bytes, &self.session.to_string())?;
        let goal = sidecar.goal.ok_or("the goal file holds no goal")?;
        Ok(Counters {
            consecutive: goal.consecutive,
            unattended: goal.unattended,
            goal_turns: goal.turns,
            toolless_streak: goal.toolless_streak,
        })
    }

    fn wake_count(&self) -> usize {
        self.script().wakes.len()
    }

    /// One tool call inside the active turn.
    async fn call_tool(&self, turn: dal_core::TurnId, id: &str) -> TestResult {
        use dal_agent::ext::Hook as _;
        super::ToolCallHook(self.runtime.clone())
            .call(
                dal_core::ext::ToolCallEvent {
                    turn,
                    call: CallId::new(id),
                    tool: dal_core::Name::parse("read")?,
                    class: dal_core::ToolClass::Read,
                    args: RawJson::parse(r#"{"path":"a"}"#)?,
                },
                HookCx::for_test(self.host.clone(), self.session, Some(turn)),
            )
            .await?;
        Ok(())
    }
}

const NO_COUNTS: Counters = Counters {
    consecutive: 0,
    unattended: 0,
    goal_turns: 0,
    toolless_streak: 0,
};

#[tokio::test(start_paused = true)]
async fn user_grace_fires_at_ten_seconds_and_a_prompt_drops_it() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture
        .user_turn(turn, Stop::EndTurn, "made progress")
        .await?;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 0, "the grace scheduled no wake yet");
    tokio::time::advance(std::time::Duration::from_millis(9_900)).await;
    fixture.pump().await;
    assert_eq!(
        fixture.wake_count(),
        0,
        "no continuation before ten seconds"
    );
    // A prompt inside the window drops the scheduled continuation.
    fixture.user_input().await?;
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    fixture.pump().await;
    assert_eq!(
        fixture.wake_count(),
        0,
        "the prompt dropped the continuation"
    );
    // The dropped prompt's own turn schedules a fresh grace, which fires.
    fixture.begin_turn(turn).await?;
    fixture
        .end_turn(turn, Stop::EndTurn, "changed direction")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 1);
    assert!(
        fixture.script().wakes[0].starts_with(GOAL_PROMPT_HEAD),
        "{}",
        fixture.script().wakes[0]
    );
    Ok(())
}

/// A goal fixture whose settled user turn armed the ten-second continuation.
async fn armed_continuation() -> Result<Fixture, Box<dyn std::error::Error>> {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    fixture
        .user_turn(goal_turn(), Stop::EndTurn, "made progress")
        .await?;
    fixture.pump().await;
    Ok(fixture)
}

/// The real input hook over `fixture`, bound to `cancel` as the opening token.
fn input_call(
    fixture: &Fixture,
    cancel: CancellationToken,
) -> dal_agent::ext::BoxFuture<
    'static,
    Result<dal_core::ext::InputVerdict, dal_agent::ext::HookError>,
> {
    use dal_agent::ext::Hook as _;
    let mut cx = HookCx::for_test(fixture.host.clone(), fixture.session, None);
    cx.cancel = cancel;
    super::InputHook(fixture.runtime.clone()).call(
        dal_core::ext::InputEvent {
            content: vec![dal_core::Part::Text {
                text: "work".into(),
            }],
            model: None,
        },
        cx,
    )
}

/// Yields until `ready` holds, so a test waits on an observed fact instead
/// of a fixed number of scheduler turns.
async fn yield_until(mut ready: impl FnMut() -> bool) -> TestResult {
    for _ in 0..10_000 {
        if ready() {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    Err("the awaited condition never held".into())
}

/// Parks the owner inside a `jobs` list and returns the parked call, so
/// every later message queues behind it.
#[expect(
    clippy::disallowed_methods,
    reason = "the held-owner witness spawns the one call the owner parks inside"
)]
async fn park_owner(
    fixture: &Fixture,
) -> Result<tokio::task::JoinHandle<Result<String, ServiceError>>, Box<dyn std::error::Error>> {
    let gate = fixture.runtime.parking_gate();
    gate.arm();
    let parked = tokio::spawn(list_agents_call(
        fixture.runtime.clone(),
        fixture.session,
        fixture.caller(),
        CallId::new("park"),
    ));
    yield_until(|| gate.parked() >= 1).await?;
    Ok(parked)
}

/// Sends the real input hook with `cancel` and returns once its message
/// sits in the held owner's mailbox.
#[expect(
    clippy::disallowed_methods,
    reason = "the held-owner witness runs the hook beside the parked owner"
)]
async fn queue_input(
    fixture: &Fixture,
    cancel: CancellationToken,
) -> Result<
    tokio::task::JoinHandle<Result<dal_core::ext::InputVerdict, dal_agent::ext::HookError>>,
    Box<dyn std::error::Error>,
> {
    let before = fixture.runtime.mailbox_fill_now(fixture.session);
    let call = tokio::spawn(input_call(fixture, cancel));
    yield_until(|| fixture.runtime.mailbox_fill_now(fixture.session) > before).await?;
    Ok(call)
}

/// Frees the held owner, then proves it drained its queue: a read-only call
/// sent after the queued input is answered only once that input was handled.
async fn release_and_drain(
    fixture: &Fixture,
    parked: tokio::task::JoinHandle<Result<String, ServiceError>>,
) -> TestResult {
    fixture.runtime.parking_gate().release();
    parked.await.map_err(|error| error.to_string())??;
    list_agents_call(
        fixture.runtime.clone(),
        fixture.session,
        fixture.caller(),
        CallId::new("drain"),
    )
    .await?;
    Ok(())
}

/// Lets the armed ten-second continuation come due and reports what the
/// battery did with it: the wakes sent and the goal counters it saved.
async fn continuation_outcome(
    fixture: &Fixture,
) -> Result<(usize, Counters), Box<dyn std::error::Error>> {
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    Ok((fixture.wake_count(), fixture.counters()?))
}

/// A prompt whose opening was cancelled while its message sat behind a busy
/// owner must leave the battery exactly as a cancel before the hook enqueued
/// anything does: the armed continuation survives and is delivered.
#[tokio::test(start_paused = true)]
async fn a_prompt_cancelled_while_queued_leaves_the_battery_as_a_preenqueue_cancel_does()
-> TestResult {
    // Baseline: the cancel is already set when the hook starts, so nothing
    // is enqueued and the owner never sees the prompt.
    let baseline = armed_continuation().await?;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        matches!(
            input_call(&baseline, cancelled).await,
            Err(dal_agent::ext::HookError::Cancelled)
        ),
        "a pre-cancelled opening never reaches the owner"
    );
    let expected = continuation_outcome(&baseline).await?;
    assert_eq!(
        expected.0, 1,
        "the baseline keeps and delivers the continuation"
    );

    // Queued: the owner is held mid-message, the prompt is enqueued behind
    // it, and the opening is cancelled only then.
    let queued = armed_continuation().await?;
    let parked = park_owner(&queued).await?;
    let token = CancellationToken::new();
    let call = queue_input(&queued, token.clone()).await?;
    token.cancel();
    assert!(
        matches!(
            call.await.map_err(|error| error.to_string())?,
            Err(dal_agent::ext::HookError::Cancelled)
        ),
        "the waiter gives up on the cancelled opening"
    );
    release_and_drain(&queued, parked).await?;
    let actual = continuation_outcome(&queued).await?;
    assert_eq!(
        actual, expected,
        "a prompt cancelled while queued must not reach the battery state"
    );
    assert!(
        queued.script().wakes[0].starts_with(GOAL_PROMPT_HEAD),
        "{}",
        queued.script().wakes[0]
    );
    Ok(())
}

/// The control for the witness above: a queued prompt whose opening is NOT
/// cancelled is still applied by the owner and drops the armed continuation,
/// so the cancellation check skips only prompts the waiter gave up on.
#[tokio::test(start_paused = true)]
async fn a_queued_prompt_with_a_live_opening_still_drops_the_continuation() -> TestResult {
    let fixture = armed_continuation().await?;
    let parked = park_owner(&fixture).await?;
    let call = queue_input(&fixture, CancellationToken::new()).await?;
    release_and_drain(&fixture, parked).await?;
    assert!(
        matches!(
            call.await.map_err(|error| error.to_string())?,
            Ok(dal_core::ext::InputVerdict::Continue)
        ),
        "a live queued prompt is answered and continues"
    );
    let (wakes, _) = continuation_outcome(&fixture).await?;
    assert_eq!(wakes, 0, "the applied prompt dropped the continuation");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_dropped_continuation_leaves_the_counters_and_a_delivered_one_counts_once() -> TestResult
{
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture
        .user_turn(turn, Stop::EndTurn, "first attempt")
        .await?;
    assert_eq!(
        fixture.counters()?,
        Counters {
            toolless_streak: 1,
            ..NO_COUNTS
        },
        "a grant counts the turn's output but no delivery"
    );
    // The prompt drops the grant at nine seconds: still nothing counted.
    tokio::time::advance(std::time::Duration::from_secs(9)).await;
    fixture.pump().await;
    fixture.user_input().await?;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 0);
    assert_eq!(
        fixture.counters()?.goal_turns,
        0,
        "a dropped continuation was never delivered"
    );
    fixture.begin_turn(turn).await?;
    fixture
        .end_turn(turn, Stop::EndTurn, "second attempt")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 1);
    let delivered = fixture.counters()?;
    assert_eq!(
        (
            delivered.consecutive,
            delivered.unattended,
            delivered.goal_turns
        ),
        (1, 1, 1),
        "one delivered continuation counts once"
    );
    // Nothing else is pending, so more time delivers and counts nothing.
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 1);
    assert_eq!(fixture.counters()?.goal_turns, 1);
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
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 1);
    let wake = fixture.script().wakes[0].clone();
    let Some(goal_at) = wake.find(GOAL_PROMPT_HEAD) else {
        return Err(format!("the wake carries no goal prompt: {wake}").into());
    };
    let Some(jobs_at) = wake.find("server exited: 2 problems left") else {
        return Err(format!("the wake carries no job report: {wake}").into());
    };
    assert!(jobs_at < goal_at, "P2 precedes P4: {wake}");
    let counted = fixture.counters()?;
    assert_eq!(
        (counted.consecutive, counted.unattended, counted.goal_turns),
        (1, 1, 1),
        "the delivered idle continuation counts once"
    );
    assert_eq!(
        counted.toolless_streak, 0,
        "an idle admission records no turn output"
    );
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
    assert_eq!(fixture.wake_count(), 0, "a failed turn schedules nothing");
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
    assert_eq!(fixture.wake_count(), 1);
    assert!(
        fixture.script().wakes[0].starts_with(GOAL_PROMPT_HEAD),
        "{}",
        fixture.script().wakes[0]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_overflowed_turn_blocks_with_the_overflow_reason_and_a_plain_failure_does_not()
-> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture.user_input().await?;
    fixture.begin_turn(turn).await?;
    fixture.end_turn_with(turn, Stop::Failed, true, "").await?;
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(
        shown.contains("blocked: context overflow ended the turn (compaction did not recover)"),
        "{shown}"
    );
    tokio::time::advance(std::time::Duration::from_secs(12)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 0, "an overflow schedules nothing");
    // The same stop without the core's overflow mark is a provider error.
    fixture.user_turn(turn, Stop::EndTurn, "retry").await?;
    fixture.user_input().await?;
    fixture.begin_turn(turn).await?;
    fixture.end_turn(turn, Stop::Failed, "").await?;
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(
        shown.contains("blocked: provider error ended the turn (retries exhausted)"),
        "{shown}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_unsavable_provider_error_block_rolls_back_and_is_reported() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    // A user prompt itself persists the goal; arm the failure after input
    // so it hits the provider-error block at turn end.
    fixture.user_input().await?;
    fixture.begin_turn(turn).await?;
    fixture.script().sidecar_write_error = Some("the sidecar store is down");
    fixture.end_turn(turn, Stop::Failed, "").await?;
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(
        !shown.contains("blocked: provider error ended the turn (retries exhausted)"),
        "memory must match the unblocked file: {shown}"
    );
    assert!(
        fixture.script().notices.iter().any(|notice| {
            notice.kind.as_ref() == "orchestration.goal"
                && notice.text.contains("the sidecar store is down")
        }),
        "the failed block is reported"
    );
    // The next provider-error stop retries the block once the store answers.
    fixture.user_turn(turn, Stop::Failed, "").await?;
    let shown = fixture.runtime.command(fixture.session, "goal", "").await?;
    assert!(
        shown.contains("blocked: provider error ended the turn (retries exhausted)"),
        "{shown}"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_unsavable_goal_delivery_requeues_before_starting_the_wake() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture
        .user_turn(turn, Stop::EndTurn, "made progress")
        .await?;
    let baseline = fixture.script().events.len();
    fixture.script().sidecar_write_error = Some("the sidecar store is down");
    tokio::time::advance(std::time::Duration::from_millis(10_100)).await;
    fixture.pump().await;
    let events = fixture.script().events[baseline..].to_vec();
    let Some(wake_at) = events.iter().position(|event| *event == "wake") else {
        return Err("the continuation was never retried".into());
    };
    assert_eq!(
        events.first(),
        Some(&"sidecar"),
        "the failed sidecar write happens before any wake attempt"
    );
    assert!(
        events[..wake_at].iter().all(|event| *event == "sidecar"),
        "the wake is not attempted until the sidecar write succeeds: {events:?}"
    );
    let delivered = fixture.counters()?;
    assert_eq!(
        (delivered.unattended, delivered.goal_turns),
        (1, 1),
        "the retried wake is counted exactly once"
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
    assert_eq!(fixture.wake_count(), 1);
    // The wake turn was automatic: its opening runs before_turn, which
    // clears the prompt latch, and its continuation waits no grace.
    fixture.begin_turn(turn).await?;
    fixture
        .end_turn(turn, Stop::EndTurn, "more progress")
        .await?;
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    assert_eq!(fixture.wake_count(), 2);
    let counted = fixture.counters()?;
    assert_eq!(
        (counted.unattended, counted.goal_turns),
        (2, 2),
        "each delivered continuation counts once"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_turn_that_called_a_tool_resets_the_toolless_streak() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let turn = goal_turn();
    fixture.user_input().await?;
    fixture.begin_turn(turn).await?;
    fixture.end_turn(turn, Stop::EndTurn, "thinking").await?;
    assert_eq!(fixture.counters()?.toolless_streak, 1);
    fixture.user_input().await?;
    fixture.begin_turn(turn).await?;
    fixture.call_tool(turn, "read-1").await?;
    fixture.end_turn(turn, Stop::EndTurn, "read a file").await?;
    assert_eq!(
        fixture.counters()?.toolless_streak,
        0,
        "the settled hook sees the tool call the turn made"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_accepted_wake_keeps_its_acknowledgement_until_the_host_confirms_every_report()
-> TestResult {
    let fixture = Fixture::open().await?;
    let (first, second) = (
        fixture.done_job("first report"),
        fixture.done_job("second report"),
    );
    fixture.script().commit_replies.extend([
        CommitReply::Fail("the jobs service is restarting"),
        CommitReply::First,
    ]);
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    for _ in 0..8 {
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        fixture.pump().await;
    }
    assert_eq!(fixture.wake_count(), 1);
    assert!(
        fixture
            .host
            .notices()
            .iter()
            .any(|notice| &*notice.kind == "orchestration.delivery"
                && notice.text.contains("the jobs service is restarting")),
        "the failed acknowledgement is reported to the owner"
    );
    assert!(fixture.script().delivered.is_empty());
    for _ in 0..3 {
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        fixture.pump().await;
    }
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1, "the wake is never repeated");
    assert_eq!(
        script.delivered,
        HashSet::from([first, second]),
        "every report is eventually confirmed"
    );
    assert!(script.taken.is_empty());
    assert_eq!(
        script.releases,
        Vec::<Vec<JobId>>::new(),
        "no release on the accepted path"
    );
    assert_eq!(script.commits.len(), 3, "failure, partial, then the rest");
    assert_eq!(script.commits[0].len(), 2);
    assert_eq!(script.commits[1].len(), 2);
    assert_eq!(
        script.commits[2].len(),
        1,
        "only the unconfirmed id is retried"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_confirmed_wake_commits_once_and_a_refused_wake_releases_and_requeues_once() -> TestResult
{
    let fixture = Fixture::open().await?;
    let job = fixture.done_job("only report");
    fixture.script().wake_refusal = Some("the turn service refused");
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    fixture.pump().await;
    for _ in 0..3 {
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        fixture.pump().await;
    }
    let script = fixture.script();
    assert_eq!(script.releases, vec![vec![job]], "released exactly once");
    assert_eq!(
        script.wakes.len(),
        1,
        "the report is requeued and woken once"
    );
    assert_eq!(script.commits, vec![vec![job]], "committed exactly once");
    assert!(script.delivered.contains(&job));
    drop(script);
    let delivery: Vec<_> = fixture
        .host
        .notices()
        .into_iter()
        .filter(|notice| &*notice.kind == "orchestration.delivery")
        .collect();
    assert_eq!(delivery.len(), 1, "the refused wake is reported once");
    assert!(
        delivery[0].text.contains("the turn service refused"),
        "{}",
        delivery[0].text
    );
    Ok(())
}

/// The core answers one wake past its automatic-turn cutoff with a
/// wake-limit denial; the battery stops its controller and keeps the report
/// queued instead of waking again. The twenty-turn count itself stays
/// core-owned: this proves only the stop-on-denial arm, never the number.
#[tokio::test(start_paused = true)]
async fn a_wake_limit_denial_stops_automatic_wakes_and_keeps_the_report() -> TestResult {
    let fixture = Fixture::open().await?;
    let job = fixture.done_job("cutoff report");
    fixture.script().wake_denied_limit = true;
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    fixture.pump().await;
    for _ in 0..6 {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        fixture.pump().await;
    }
    {
        let script = fixture.script();
        assert_eq!(
            script
                .events
                .iter()
                .filter(|event| **event == "wake")
                .count(),
            1,
            "the denied wake is attempted once, never retried: the controller stopped"
        );
        assert!(
            script.wakes.is_empty(),
            "no wake reached the session past the cutoff"
        );
        assert_eq!(script.releases, vec![vec![job]], "released exactly once");
        assert!(
            script.commits.is_empty(),
            "an undelivered report is never committed"
        );
        assert!(
            !script.delivered.contains(&job),
            "the report stays queued for the next user turn"
        );
    }
    let delivery: Vec<_> = fixture
        .host
        .notices()
        .into_iter()
        .filter(|notice| &*notice.kind == "orchestration.delivery")
        .collect();
    assert!(
        delivery.is_empty(),
        "a cutoff stop is silent: the report waits, no failure is reported"
    );
    // The stop persists: a second report ending later starts no new wake.
    // The one-shot denial is consumed, so any further wake would succeed;
    // a paused controller never attempts one.
    let second = fixture.done_job("later report");
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    fixture.pump().await;
    for _ in 0..6 {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        fixture.pump().await;
    }
    let script = fixture.script();
    assert_eq!(
        script
            .events
            .iter()
            .filter(|event| **event == "wake")
            .count(),
        1,
        "the cutoff stop persists: the later report starts no new wake"
    );
    assert!(
        script.wakes.is_empty(),
        "no wake reached the session past the cutoff"
    );
    assert!(
        !script.delivered.contains(&second),
        "the later report also waits for the next user turn"
    );
    drop(script);
    let delivery: Vec<_> = fixture
        .host
        .notices()
        .into_iter()
        .filter(|notice| &*notice.kind == "orchestration.delivery")
        .collect();
    assert!(
        delivery.is_empty(),
        "the persistent stop stays silent: no failure is reported"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_failed_delivery_poll_is_reported_once_to_the_owner() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.done_job("doomed report");
    {
        let mut script = fixture.script();
        script.wake_refusal = Some("the turn service refused");
        script.release_fail = Some("the jobs service is restarting");
    }
    tokio::time::advance(std::time::Duration::from_millis(300)).await;
    fixture.pump().await;
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    fixture.pump().await;
    for _ in 0..3 {
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        fixture.pump().await;
    }
    let delivery: Vec<_> = fixture
        .host
        .notices()
        .into_iter()
        .filter(|notice| &*notice.kind == "orchestration.delivery")
        .collect();
    assert_eq!(delivery.len(), 1, "one notice per distinct cause");
    assert!(
        delivery[0].text.contains("the jobs service is restarting"),
        "{}",
        delivery[0].text
    );
    Ok(())
}

impl Fixture {
    /// Ends `count` top-level jobs; sixteen waiting reports wake the session
    /// at the first delivery tick, without the two-second quiet wait.
    fn done_jobs(&self, count: usize, text: impl Fn(usize) -> String) -> Vec<JobId> {
        (0..count)
            .map(|index| self.done_job(&text(index)))
            .collect()
    }

    /// Nine identical calls trip the loop guard, which admits its recovery
    /// text for the next wake.
    async fn trip_loop_guard(&self) -> TestResult {
        let turn = goal_turn();
        self.begin_turn(turn).await?;
        for index in 0..9 {
            self.call_tool(turn, &format!("loop-{index}")).await?;
        }
        Ok(())
    }

    /// Runs `quarters` delivery ticks, one quarter-second of virtual time
    /// each.
    async fn run_ticks(&self, quarters: u32) {
        for _ in 0..quarters {
            tokio::time::advance(std::time::Duration::from_millis(250)).await;
            self.pump().await;
        }
    }

    fn delivery_notices(&self) -> Vec<Notice> {
        let notices = self.host.notices();
        notices
            .into_iter()
            .filter(|notice| &*notice.kind == "orchestration.delivery")
            .collect()
    }
}

const RECOVERY_HEAD: &str = "The loop guard stopped the previous turn";

/// A refused wake whose release also fails must not lose the reports: the
/// host still counts them as taken, so only the owner's held set can offer
/// them again. Before the repair the release error returned before the
/// requeue, `held` had been drained for the wake, and the scripted host never
/// re-offers a taken report: no second wake ever came.
#[tokio::test(start_paused = true)]
async fn a_failed_release_after_a_refused_wake_keeps_the_report_for_the_next_wake() -> TestResult {
    let fixture = Fixture::open().await?;
    let job = fixture.done_job("redelivered report");
    {
        let mut script = fixture.script();
        script.wake_refusal = Some("the turn service refused");
        script.release_fail = Some("the jobs service is restarting");
    }
    fixture.run_ticks(14).await;
    let notices = fixture.delivery_notices();
    assert_eq!(notices.len(), 1, "the failed release is reported once");
    assert!(
        notices[0].text.contains("the jobs service is restarting"),
        "the caller still sees the release error: {}",
        notices[0].text
    );
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1, "one wake follows the failure");
    assert!(
        script.wakes[0].contains("redelivered report"),
        "{}",
        script.wakes[0]
    );
    assert_eq!(script.commits, vec![vec![job]], "committed exactly once");
    assert!(script.delivered.contains(&job));
    assert!(
        script.releases.is_empty(),
        "the failed release is the only one"
    );
    Ok(())
}

/// The recovery text that rode with the reports is a source with no job
/// behind it: `collect` drained it from the arbiter, so a failed release must
/// still requeue it. Before the repair the recovery text was dropped with the
/// reports and no wake followed.
#[tokio::test(start_paused = true)]
async fn a_failed_release_after_a_refused_wake_requeues_the_recovery_text_with_the_reports()
-> TestResult {
    let fixture = Fixture::open().await?;
    let jobs = fixture.done_jobs(16, |index| format!("report <{index}>"));
    fixture.trip_loop_guard().await?;
    {
        let mut script = fixture.script();
        script.wake_refusal = Some("the turn service refused");
        script.release_fail = Some("the jobs service is restarting");
    }
    fixture.run_ticks(8).await;
    let notices = fixture.delivery_notices();
    assert_eq!(notices.len(), 1, "the failed release is reported once");
    assert!(
        notices[0].text.contains("the jobs service is restarting"),
        "{}",
        notices[0].text
    );
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1, "one wake carries everything");
    let wake = &script.wakes[0];
    assert!(wake.contains(RECOVERY_HEAD), "recovery text lost: {wake}");
    for index in 0..16 {
        assert!(wake.contains(&format!("report <{index}>")), "{wake}");
    }
    assert_eq!(script.delivered, jobs.into_iter().collect::<HashSet<_>>());
    Ok(())
}

/// The goal checkpoint path: the sidecar write fails before the wake starts
/// and the release of the wake's reports fails too. The continuation and the
/// report must both come back for the next wake.
#[tokio::test(start_paused = true)]
async fn a_failed_release_after_an_unsavable_goal_checkpoint_keeps_the_report_and_the_goal()
-> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let job = fixture.done_job("checkpoint report");
    {
        let mut script = fixture.script();
        script.sidecar_write_error = Some("the sidecar store is down");
        script.release_fail = Some("the jobs service is restarting");
    }
    fixture.run_ticks(14).await;
    let notices = fixture.delivery_notices();
    assert_eq!(notices.len(), 1, "the failed release is reported once");
    assert!(
        notices[0].text.contains("the jobs service is restarting"),
        "{}",
        notices[0].text
    );
    let counted = fixture.counters()?;
    assert_eq!(
        (counted.consecutive, counted.unattended, counted.goal_turns),
        (1, 1, 1),
        "the retried continuation counts once"
    );
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1);
    assert!(script.wakes[0].contains("checkpoint report"));
    assert!(script.wakes[0].contains(GOAL_PROMPT_HEAD));
    assert!(script.delivered.contains(&job));
    Ok(())
}

/// The omitted-report release runs before any wake: a source composed with
/// reports that overflow the budget loses the recovery text and every
/// report when that release fails, since `collect` and the held drain have
/// already happened.
#[tokio::test(start_paused = true)]
async fn a_failed_release_of_omitted_reports_keeps_the_recovery_text_and_every_report() -> TestResult
{
    let fixture = Fixture::open().await?;
    let jobs = fixture.done_jobs(16, |index| format!("{index:02} {}", "x".repeat(1_100)));
    fixture.trip_loop_guard().await?;
    fixture.script().release_fail = Some("the jobs service is restarting");
    fixture.run_ticks(14).await;
    let notices = fixture.delivery_notices();
    assert_eq!(notices.len(), 1, "the failed release is reported once");
    assert!(
        notices[0].text.contains("the jobs service is restarting"),
        "{}",
        notices[0].text
    );
    let script = fixture.script();
    assert_eq!(script.delivered, jobs.into_iter().collect::<HashSet<_>>());
    assert_eq!(
        script.wakes.len(),
        2,
        "the budget splits the sixteen reports over two wakes"
    );
    assert!(script.wakes[0].contains(RECOVERY_HEAD));
    assert!(!script.wakes[1].contains(RECOVERY_HEAD));
    assert_eq!(
        script.releases.len(),
        1,
        "the omitted reports are released once after the failure"
    );
    Ok(())
}

/// The goal verdict fails while sixteen fresh reports are due, and the
/// release that returns them fails too: restoring `held` must not wait for
/// the release.
#[tokio::test(start_paused = true)]
async fn a_failed_release_after_a_failed_goal_verdict_keeps_the_fresh_reports() -> TestResult {
    let fixture = Fixture::open_goal().await?;
    fixture.create_goal("write the parser").await?;
    let jobs = fixture.done_jobs(16, |index| format!("report <{index}>"));
    {
        let mut script = fixture.script();
        // The delivery poll lists once itself; the verdict's live-work count
        // lists again and fails.
        script.jobs_list_skip = 1;
        script.jobs_list_error = Some("the jobs list is unavailable");
        script.release_fail = Some("the jobs service is restarting");
    }
    fixture.run_ticks(8).await;
    let notices = fixture.delivery_notices();
    assert_eq!(notices.len(), 1, "the failed release is reported once");
    assert!(
        notices[0].text.contains("the jobs service is restarting"),
        "{}",
        notices[0].text
    );
    let counted = fixture.counters()?;
    assert_eq!(
        (counted.consecutive, counted.unattended, counted.goal_turns),
        (1, 1, 1)
    );
    let script = fixture.script();
    assert_eq!(script.wakes.len(), 1);
    assert!(script.wakes[0].contains(GOAL_PROMPT_HEAD));
    assert_eq!(script.delivered, jobs.into_iter().collect::<HashSet<_>>());
    Ok(())
}

const ISOLATED_WRITE: &str = r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#;

type GitAnswer = Result<RunOutput, &'static str>;

/// The git answers of one isolated task up to its saved patch: preflight,
/// the worktree, the staged diff, and the changed names.
fn isolated_prefix() -> Vec<GitAnswer> {
    isolated_prefix_for("code\n", "diff --git a/code b/code\n")
}

/// The scripted repository top level: the platform temp dir the test
/// workspaces live under, so the fixtures hold under any TMPDIR.
fn temp_toplevel() -> String {
    std::env::temp_dir().display().to_string()
}

/// One isolated task's scripted git answers with its own in-scope changed
/// names and diff, for the sessions that share one checkout merge lock.
fn isolated_prefix_for(changed: &str, diff: &str) -> Vec<GitAnswer> {
    isolated_prefix_for_top(&temp_toplevel(), changed, diff)
}

/// One isolated task's scripted git answers against an explicit repository
/// top, for sessions whose checkouts are distinct merge keys.
fn isolated_prefix_for_top(top: &str, changed: &str, diff: &str) -> Vec<GitAnswer> {
    vec![
        Ok(output("git version 2.47.0")),
        Ok(output(top)),
        Ok(output("head")),
        Ok(output("snapshot")),
        Ok(output("")),
        Ok(output("")),
        Ok(output(diff)),
        Ok(output(changed)),
    ]
}

fn failed_output(stderr: &str) -> RunOutput {
    RunOutput {
        status: dal_core::ExitStatusKind::Exited(1),
        stderr_tail: stderr.as_bytes().to_vec(),
        ..output("")
    }
}

#[tokio::test(start_paused = true)]
async fn sessions_on_one_workspace_apply_their_patches_one_at_a_time() -> TestResult {
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    let runtime = Runtime::new(config)?;
    let gate = Arc::new(ApplyGate::new());
    let first = Fixture::open_on(runtime.clone(), Some(Arc::clone(&gate))).await?;
    let second = Fixture::open_on(runtime.clone(), Some(Arc::clone(&gate))).await?;
    for fixture in [&first, &second] {
        let mut script = fixture.script();
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    first.tool(ISOLATED_WRITE).await?;
    second.tool(ISOLATED_WRITE).await?;
    let window = std::time::Duration::from_secs(5);
    tokio::time::timeout(window, gate.entered(1)).await?;
    let Some(ApplyEvent::Enter(holder)) = gate.events().first().copied() else {
        return Err("no merge reached apply".into());
    };
    let waiter = if holder == first.session {
        second.session
    } else {
        first.session
    };
    let overlapped = tokio::time::timeout(std::time::Duration::from_secs(1), gate.entered(2)).await;
    assert!(
        overlapped.is_err(),
        "the second merge reached apply while the first held the checkout"
    );
    gate.release.add_permits(1);
    tokio::time::timeout(window, gate.entered(2)).await?;
    gate.release.add_permits(1);
    let (first_report, second_report) = (first.ended_run().await?, second.ended_run().await?);
    assert!(first_report.contains("isolation: merged"), "{first_report}");
    assert!(
        second_report.contains("isolation: merged"),
        "{second_report}"
    );
    assert_eq!(
        gate.events(),
        [
            ApplyEvent::Enter(holder),
            ApplyEvent::Exit(holder),
            ApplyEvent::Enter(waiter),
            ApplyEvent::Exit(waiter),
        ]
    );
    assert_eq!(runtime.merge_locks.len(), 0);
    Ok(())
}

#[derive(Deserialize)]
struct RetainedRecord {
    reason: String,
    base: String,
    worktree: String,
}

impl Fixture {
    fn git_calls(&self, matches: impl Fn(&[std::ffi::OsString]) -> bool) -> Vec<Vec<String>> {
        self.script()
            .run_requests
            .iter()
            .filter(|request| matches(&request.argv))
            .map(|request| {
                request
                    .argv
                    .iter()
                    .map(|word| word.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    fn git_verb_calls(&self, verb: &str) -> Vec<Vec<String>> {
        self.git_calls(|argv| argv.iter().any(|word| word == verb))
    }

    fn plain_apply_calls(&self) -> usize {
        self.git_calls(is_plain_apply).len()
    }

    fn worktree_adds(&self) -> usize {
        self.git_calls(|argv| {
            argv.iter().any(|word| word == "worktree") && argv.iter().any(|word| word == "add")
        })
        .len()
    }

    fn task_artifact(&self, file: ArtifactFile) -> Result<Vec<u8>, &'static str> {
        let script = self.script();
        let task = script
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_some())
            .ok_or("task absent")?
            .0
            .id;
        script
            .artifacts
            .iter()
            .find(|(job, saved, _)| *job == task && *saved == file)
            .map(|(_, _, bytes)| bytes.clone())
            .ok_or("artifact absent")
    }

    fn task_text(&self) -> Result<String, &'static str> {
        self.script()
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_some())
            .map(|(_, _, text)| text.clone())
            .ok_or("task absent")
    }
}

/// Runs one isolated task whose patch the checkout refuses: `tail` answers
/// the git calls from the apply check on.
async fn retained_run(
    tail: Vec<GitAnswer>,
) -> Result<(Fixture, String), Box<dyn std::error::Error>> {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    {
        let mut script = fixture.script();
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend(tail);
    }
    fixture.tool(ISOLATED_WRITE).await?;
    let report = fixture.ended_run().await?;
    Ok((fixture, report))
}

/// The worktree path and its retained target of the one `git worktree move`.
fn moved_worktree(fixture: &Fixture) -> Result<(String, String), &'static str> {
    let moves = fixture.git_verb_calls("move");
    let [argv] = moves.as_slice() else {
        return Err("exactly one worktree move is expected");
    };
    Ok((argv[5].clone(), argv[6].clone()))
}

#[tokio::test]
async fn a_refused_apply_check_retains_the_worktree_and_never_applies() -> TestResult {
    let (fixture, report) = retained_run(vec![
        Ok(failed_output("error: patch failed: code:1")),
        Ok(output("")),
        Ok(output("")),
    ])
    .await?;
    assert!(report.contains("isolation: retained"), "{report}");
    assert!(!report.contains("isolation: merged"), "{report}");
    assert_eq!(fixture.plain_apply_calls(), 0);
    assert_eq!(fixture.git_verb_calls("remove"), Vec::<Vec<String>>::new());
    assert!(
        fixture
            .task_artifact(ArtifactFile::DeltaPatch)?
            .starts_with(b"diff")
    );
    let (dir, target) = moved_worktree(&fixture)?;
    assert!(target.starts_with(&format!("{dir}.retained-")), "{target}");
    assert_eq!(fixture.git_verb_calls("prune").len(), 1);
    let record: RetainedRecord =
        sonic_rs::from_slice(&fixture.task_artifact(ArtifactFile::RetainedJson)?)?;
    assert_eq!(
        record.reason,
        "the changes did not apply cleanly (error: patch failed: code:1)"
    );
    assert_eq!(record.base, "snapshot");
    assert_eq!(record.worktree, target);
    let text = fixture.task_text()?;
    assert!(
        text.contains(&format!("isolation: retained at {target}")),
        "{text}"
    );
    assert!(text.contains("error: patch failed: code:1"), "{text}");
    assert!(text.contains("apply --3way"), "{text}");
    assert_eq!(fixture.runtime.merge_locks.len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_refused_apply_keeps_the_worktree_in_place_when_the_move_fails() -> TestResult {
    let (fixture, report) = retained_run(vec![
        Ok(output("")),
        Ok(failed_output("error: patch does not apply")),
        Ok(failed_output("fatal: cannot move")),
    ])
    .await?;
    assert!(report.contains("isolation: retained"), "{report}");
    assert!(!report.contains("isolation: merged"), "{report}");
    assert_eq!(fixture.plain_apply_calls(), 1);
    assert_eq!(fixture.git_verb_calls("remove"), Vec::<Vec<String>>::new());
    assert_eq!(fixture.git_verb_calls("prune"), Vec::<Vec<String>>::new());
    assert!(
        fixture
            .task_artifact(ArtifactFile::DeltaPatch)?
            .starts_with(b"diff")
    );
    let (dir, _) = moved_worktree(&fixture)?;
    let record: RetainedRecord =
        sonic_rs::from_slice(&fixture.task_artifact(ArtifactFile::RetainedJson)?)?;
    assert_eq!(
        record.worktree, dir,
        "a failed move leaves the tree where it is"
    );
    assert_eq!(
        record.reason,
        "the changes did not apply cleanly (error: patch does not apply)"
    );
    let text = fixture.task_text()?;
    assert!(
        text.contains(&format!("isolation: retained at {dir}")),
        "{text}"
    );
    assert!(text.contains("error: patch does not apply"), "{text}");
    assert!(
        text.contains("the worktree could not be moved: fatal: cannot move"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_prune_still_retains_the_moved_worktree_and_says_so() -> TestResult {
    let (fixture, report) = retained_run(vec![
        Ok(output("")),
        Ok(failed_output("error: conflict in code")),
        Ok(output("")),
        Ok(failed_output("fatal: prune denied")),
    ])
    .await?;
    assert!(report.contains("isolation: retained"), "{report}");
    assert!(!report.contains("isolation: merged"), "{report}");
    assert_eq!(fixture.plain_apply_calls(), 1);
    assert_eq!(fixture.git_verb_calls("remove"), Vec::<Vec<String>>::new());
    let (_, target) = moved_worktree(&fixture)?;
    let record: RetainedRecord =
        sonic_rs::from_slice(&fixture.task_artifact(ArtifactFile::RetainedJson)?)?;
    assert_eq!(record.worktree, target);
    let text = fixture.task_text()?;
    assert!(text.contains("error: conflict in code"), "{text}");
    assert!(
        text.contains("stale worktree records remain: fatal: prune denied"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_step_starts_no_dependent_step_and_no_task_job_for_it() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().report =
        Some("item\n".repeat(crate::orchestration::pool::ITEM_LINES_LIMIT + 1));
    fixture.tool(r#"{"action":"run","steps":[{"name":"source","prompt":"list items","tools":["read"],"isolation":"shared"},{"name":"pool","prompt":"{{item}}","items_from":"source","after":["source"],"tools":["read"],"isolation":"shared"},{"name":"tail","prompt":"wrap up","after":["pool"],"tools":["read"],"isolation":"shared"}]}"#).await?;
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 1, "only the source step ran a child");
    assert_eq!(
        script
            .jobs
            .iter()
            .filter(|(_, parent, _)| parent.is_some())
            .count(),
        1,
        "a step behind the failure creates no task job"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_aborted_between_steps_settles_cancelled_not_done() -> TestResult {
    let fixture = Fixture::open().await?;
    fixture.script().abort_on_settle = true;
    fixture.tool(r#"{"action":"run","steps":[{"name":"first","prompt":"one","tools":["read"],"isolation":"shared"},{"name":"second","prompt":"two","after":["first"],"tools":["read"],"isolation":"shared"}]}"#).await?;
    fixture.ended_run().await?;
    let script = fixture.script();
    assert_eq!(script.starts.len(), 1, "the second step never starts");
    let run = script
        .jobs
        .iter()
        .find(|(_, parent, _)| parent.is_none())
        .ok_or("run absent")?
        .0
        .state
        .clone();
    assert_eq!(
        run,
        JobStateView::Done(dal_core::JobOutcome::Cancelled),
        "an aborted run settles cancelled, not done"
    );
    Ok(())
}

#[tokio::test]
async fn queued_worktree_items_create_no_checkout_until_a_worker_is_free() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    let hold = CancellationToken::new();
    {
        let mut script = fixture.script();
        script.hold = Some(hold.clone());
        script
            .run_outputs
            .extend(isolated_prefix().into_iter().take(4));
        script.run_outputs.extend((0..40).map(|_| Ok(output(""))));
    }
    fixture.tool(r#"{"action":"run","steps":[{"name":"pool","prompt":"edit {{item}}","items":["one","two","three","four"],"workers":2,"tools":["patch"],"isolation":"worktree"}]}"#).await?;
    wait_for_starts(&fixture, 2).await?;
    assert_eq!(
        fixture.worktree_adds(),
        2,
        "only the active workers hold a checkout"
    );
    hold.cancel();
    let report = fixture.ended_run().await?;
    assert_eq!(fixture.script().starts.len(), 4);
    assert_eq!(fixture.worktree_adds(), 4);
    assert!(report.contains("done"), "{report}");
    Ok(())
}

#[tokio::test]
async fn a_task_cancelled_while_queued_for_a_worker_never_starts() -> TestResult {
    let fixture = Fixture::open_config(isolation_config()?).await?;
    let hold = CancellationToken::new();
    {
        let mut script = fixture.script();
        script.hold = Some(hold.clone());
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend((0..40).map(|_| Ok(output(""))));
    }
    fixture.tool(r#"{"action":"run","steps":[{"name":"pool","prompt":"edit {{item}}","items":["one","two"],"workers":1,"tools":["patch"],"isolation":"worktree"}]}"#).await?;
    wait_for_starts(&fixture, 1).await?;
    // Both task jobs exist once the second item parks in the slot wait.
    let tasks = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let tasks = fixture
                .script()
                .jobs
                .iter()
                .filter(|(_, parent, _)| parent.is_some())
                .map(|(job, _, _)| job.id)
                .collect::<Vec<_>>();
            if tasks.len() == 2 {
                return tasks;
            }
            fixture.host.changed.notified().await;
        }
    })
    .await
    .map_err(|_| "the queued task job never appeared")?;
    assert!(
        fixture.script().starts[0]
            .1
            .name
            .as_deref()
            .is_some_and(|name| name.contains("one")),
        "task jobs spawn in item order"
    );
    // Let the owner file both task registrations before the cancel lands.
    fixture.pump().await;
    let reply = fixture
        .tool(&format!(r#"{{"action":"cancel","ids":["{}"]}}"#, tasks[1]))
        .await?;
    assert!(reply.contains("cancelled 1"), "{reply}");
    hold.cancel();
    fixture.ended_run().await?;
    assert_eq!(
        fixture.script().starts.len(),
        1,
        "the cancelled item never started"
    );
    assert_eq!(
        fixture.worktree_adds(),
        1,
        "the cancelled item holds no checkout"
    );
    let settled = fixture
        .script()
        .jobs
        .iter()
        .find(|(job, _, _)| job.id == tasks[1])
        .is_some_and(|(job, _, text)| {
            matches!(
                job.state,
                JobStateView::Done(dal_core::JobOutcome::Cancelled)
            ) && text.contains("cancelled")
        });
    assert!(settled, "the queued item settles cancelled");
    Ok(())
}

mod step_tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use dal_agent::ext::{Scope, ToolCx};
    use dal_core::ScopeSpec;
    use dal_core::{Budget, CallId, OnError, SessionId};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::super::super::pool::IndexCollector;
    use super::super::super::workflow::{Isolation, Items, Step};
    use super::super::merge_locks::MergeLocks;
    use super::super::{Coordinator, LiveTasks};
    use crate::work::support::FakeServices;

    fn task_step(name: &str) -> Step {
        Step {
            name: name.to_owned(),
            prompt: "work".to_owned(),
            items: Items::Task,
            workers: 1,
            after: Vec::new(),
            tools: Vec::new(),
            model: None,
            role: None,
            system: None,
            isolation: Isolation::Shared,
        }
    }

    fn coordinator(services: Arc<FakeServices>) -> Coordinator {
        let caller = ToolCx::for_test(services.clone()).caller().clone();
        let (sender, _receiver) = mpsc::channel(8);
        Coordinator {
            services,
            caller,
            sender,
            session: SessionId::new_v7(),
            workspace: std::path::PathBuf::from("/tmp"),
            data_root: None,
            base: None,
            run: dal_core::JobId::new_v7(),
            call: CallId::new("c"),
            label: "run".into(),
            input: None,
            cancel: CancellationToken::new(),
            tasks: Mutex::new(LiveTasks::default()),
            merge_locks: MergeLocks::default(),
            reports: Arc::default(),
            child_max_steps: 0,
            child_max_minutes: 0,
        }
    }

    /// Waits for the scope task to forward the queued `agents.start` to the
    /// scripted host. `start_item` returns once the child handle exists; the
    /// host call itself is delivered on the scope's worker.
    async fn started_names(services: &Arc<FakeServices>) -> Vec<String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while services.agent_start_names().is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "no agents.start reached the scripted host"
            );
            tokio::task::yield_now().await;
        }
        services.agent_start_names()
    }

    #[test]
    fn worktree_child_refuses_missing_isolation_context() {
        let services = Arc::new(FakeServices::default());
        let coordinator = coordinator(services);
        let mut step = task_step("write");
        step.isolation = Isolation::Worktree;
        assert_eq!(
            coordinator
                .child_start(&step, None, &[], 0, "write 1", None)
                .err()
                .as_deref(),
            Some("worktree isolation requires a task worktree")
        );
        assert_eq!(
            coordinator
                .child_start(
                    &step,
                    None,
                    &[],
                    0,
                    "write 1",
                    Some(std::env::temp_dir().as_path()),
                )
                .err()
                .as_deref(),
            Some("worktree isolation requires a verified base")
        );
        let shared = coordinator
            .child_start(&task_step("read"), None, &[], 0, "read 1", None)
            .expect("shared steps may inherit the parent checkout");
        assert!(shared.workspace.is_none());
    }

    /// The session store refuses names made only of session-id characters;
    /// a Task step keeps its counter in the child name so a hex-only step
    /// name still admits. A regression here returns `Cancelled` from the
    /// real backend and the step never runs.
    #[tokio::test]
    async fn a_task_step_mints_a_numbered_child_name() {
        let services = Arc::new(FakeServices::default());
        let coordinator = coordinator(Arc::clone(&services));
        let scope = Scope::over(
            Arc::clone(&coordinator.services),
            &coordinator.caller,
            coordinator.cancel.child_token(),
            ScopeSpec {
                limit: 1,
                on_error: OnError::Settle,
                budget: Budget::default(),
            },
        )
        .expect("scope");
        let mut collector = IndexCollector::new(1);
        let mut pending = HashMap::new();
        coordinator
            .start_item(
                &task_step("a"),
                &[],
                (0, None),
                &scope,
                &mut collector,
                &mut pending,
            )
            .await
            .expect("the child start was built");
        assert_eq!(started_names(&services).await, ["a 1"]);
    }

    #[tokio::test]
    async fn pool_items_keep_the_item_label_shape() {
        let services = Arc::new(FakeServices::default());
        let coordinator = coordinator(Arc::clone(&services));
        let scope = Scope::over(
            Arc::clone(&coordinator.services),
            &coordinator.caller,
            coordinator.cancel.child_token(),
            ScopeSpec {
                limit: 1,
                on_error: OnError::Settle,
                budget: Budget::default(),
            },
        )
        .expect("scope");
        let mut step = task_step("a");
        step.items = Items::Literal(vec!["one".to_owned(), "two".to_owned()]);
        let mut collector = IndexCollector::new(1);
        let mut pending = HashMap::new();
        coordinator
            .start_item(
                &step,
                &[],
                (0, Some("one")),
                &scope,
                &mut collector,
                &mut pending,
            )
            .await
            .expect("the child start was built");
        assert_eq!(started_names(&services).await, ["a 1: one"]);
    }
}

#[test]
fn symlinked_workspace_mismatch_refuses_instead_of_widening() {
    let top = std::path::Path::new("/repo");
    let child = std::path::Path::new("/repo/sub");
    assert_eq!(
        super::workspace_relative(child, top),
        Some(std::path::PathBuf::from("sub"))
    );
    assert_eq!(
        super::workspace_relative(top, top),
        Some(std::path::PathBuf::new())
    );
    assert_eq!(
        super::workspace_relative(std::path::Path::new("/other"), top),
        None,
        "a workspace outside the top must refuse, never empty to the root"
    );
}
#[test]
fn raw_non_utf8_names_never_match_the_replacement_character_workspace() {
    let replacement = char::from_u32(0xFFFD).expect("the replacement character exists");
    let relative = std::path::PathBuf::from(replacement.to_string());
    if let Some(changed) = super::parse_nul_names(b"\xff/outside.txt\0") {
        assert!(
            !super::changed_within_workspace(&changed, &relative),
            "raw 0xFF must not read as the workspace U+FFFD: {changed:?}"
        );
    }
    assert!(
        super::parse_nul_names(b"sub/good.txt\0").is_some_and(|changed| {
            super::changed_within_workspace(&changed, std::path::Path::new("sub"))
        }),
        "valid nested names must stay in scope"
    );
}

#[test]
fn kept_capture_failure_reports_its_recovery_path() {
    let dir = std::path::Path::new("/tmp/data/worktrees/s/t");
    let end = super::kept_without_patch("the changes could not be staged", dir);
    assert!(
        end.notice.contains("/tmp/data/worktrees/s/t"),
        "kept notice must carry the recovery path: {}",
        end.notice
    );
    assert!(
        end.notice.contains("the changes could not be staged"),
        "kept notice must keep the reason: {}",
        end.notice
    );
    assert!(
        end.changed.is_empty(),
        "kept without a patch has no changed paths"
    );
    match end.isolation {
        Some(super::super::worktree::IsolationOutcome::Kept {
            patch: None,
            reason: Some(reason),
        }) => {
            assert!(
                reason.contains("the changes could not be staged"),
                "{reason}"
            );
        }
        other => panic!("kept without a patch must stay Kept with a reason: {other:?}"),
    }
}

#[tokio::test]
async fn merge_locks_keyed_by_checkout_share_subdirectories() {
    let locks = super::merge_locks::MergeLocks::default();
    let top = std::path::Path::new("/tmp/repo-top");
    let first = locks.hold(top).await;
    assert_eq!(
        locks.len(),
        1,
        "one checkout holds one lock while its merge runs"
    );
    drop(first);
    assert_eq!(
        locks.len(),
        0,
        "the registry forgets the slot after release"
    );
}

#[tokio::test(start_paused = true)]
async fn sessions_on_different_subdirectories_of_one_checkout_apply_one_at_a_time() -> TestResult {
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    let runtime = Runtime::new(config)?;
    let gate = Arc::new(ApplyGate::new());
    let root = std::env::temp_dir();
    tokio::fs::create_dir_all(root.join("one")).await?;
    tokio::fs::create_dir_all(root.join("two")).await?;
    let one = dal_core::Workspace::new(root.join("one")).expect("absolute workspace");
    let two = dal_core::Workspace::new(root.join("two")).expect("absolute workspace");
    let first = Fixture::open_at(runtime.clone(), Some(Arc::clone(&gate)), one).await?;
    let second = Fixture::open_at(runtime.clone(), Some(Arc::clone(&gate)), two).await?;
    {
        let mut script = first.script();
        script.run_outputs.extend(isolated_prefix_for(
            "one/code\n",
            "diff --git a/one/code b/one/code\n",
        ));
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    {
        let mut script = second.script();
        script.run_outputs.extend(isolated_prefix_for(
            "two/code\n",
            "diff --git a/two/code b/two/code\n",
        ));
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    first.tool(ISOLATED_WRITE).await?;
    second.tool(ISOLATED_WRITE).await?;
    let window = std::time::Duration::from_secs(5);
    tokio::time::timeout(window, gate.entered(1)).await?;
    let Some(ApplyEvent::Enter(holder)) = gate.events().first().copied() else {
        return Err("no merge reached apply".into());
    };
    let waiter = if holder == first.session {
        second.session
    } else {
        first.session
    };
    let overlapped = tokio::time::timeout(std::time::Duration::from_secs(1), gate.entered(2)).await;
    assert!(
        overlapped.is_err(),
        "the second checkout merge reached apply while the first held it"
    );
    gate.release.add_permits(1);
    tokio::time::timeout(window, gate.entered(2)).await?;
    gate.release.add_permits(1);
    let (first_report, second_report) = (first.ended_run().await?, second.ended_run().await?);
    assert!(first_report.contains("isolation: merged"), "{first_report}");
    assert!(
        second_report.contains("isolation: merged"),
        "{second_report}"
    );
    assert_eq!(
        gate.events(),
        [
            ApplyEvent::Enter(holder),
            ApplyEvent::Exit(holder),
            ApplyEvent::Enter(waiter),
            ApplyEvent::Exit(waiter),
        ]
    );
    assert_eq!(runtime.merge_locks.len(), 0);
    let _ = tokio::fs::remove_dir_all(root.join("one")).await;
    let _ = tokio::fs::remove_dir_all(root.join("two")).await;
    Ok(())
}

/// Builds a unique temp root for real-git boundary fixtures.
fn unique_temp_root(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ))
}

/// Runs one git argv in `dir` with a hermetic config, asserting success.
#[expect(
    clippy::disallowed_methods,
    reason = "the boundary test intentionally runs the real git executable"
)]
fn run_git(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?} failed");
    output
}

/// Runs one computed git argv with a hermetic config, asserting success.
#[expect(
    clippy::disallowed_methods,
    reason = "the boundary test intentionally runs the real git executable"
)]
fn run_git_argv(argv: &[String]) -> std::process::Output {
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {argv:?} failed");
    output
}

/// Inits `root` with a seed commit and returns its HEAD.
fn seed_new_repo(root: &std::path::Path) -> String {
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "test"]);
    std::fs::write(root.join("seed.txt"), b"seed").expect("seed");
    run_git(root, &["add", "-A"]);
    run_git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-m", "seed"],
    );
    let base = run_git(root, &["rev-parse", "HEAD"]);
    let base = String::from_utf8(base.stdout).expect("head utf-8");
    base.trim().to_owned()
}

/// Returns the saved diff and NUL-names texts of `base` in `root`.
fn cached_texts(root_text: &str, base: &str) -> (String, String) {
    let diff_argv = super::super::worktree::argv_cached_diff(root_text, base);
    let names_argv = super::super::worktree::argv_cached_names(root_text, base);
    let diff_out = run_git_argv(&diff_argv);
    let names_out = run_git_argv(&names_argv);
    let diff_text = String::from_utf8(diff_out.stdout).expect("diff utf-8");
    let names_text = String::from_utf8(names_out.stdout).expect("names utf-8");
    (diff_text, names_text)
}

/// The twelve scripted git answers of one merged isolated task.
fn merged_script_outputs(
    root_text: &str,
    base: &str,
    diff_text: &str,
    names_text: &str,
) -> Vec<GitAnswer> {
    vec![
        Ok(output("git version 2.47.0")),
        Ok(output(root_text)),
        Ok(output(base)),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
        Ok(output(diff_text)),
        Ok(output(names_text)),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
    ]
}

#[tokio::test]
async fn newline_tab_and_quote_names_survive_the_saved_changed_list() -> TestResult {
    let setup = tokio::task::spawn_blocking(newline_setup)
        .await
        .expect("setup joins");
    let (root_text, base, diff_text, names_text, wanted) = setup;
    let workspace = dal_core::Workspace::new(std::path::PathBuf::from(root_text.as_str()))
        .expect("absolute workspace");
    let fixture = Fixture::open_at(Runtime::new(isolation_config()?)?, None, workspace).await?;
    {
        let mut script = fixture.script();
        script.run_outputs.extend(merged_script_outputs(
            &root_text,
            &base,
            &diff_text,
            &names_text,
        ));
    }
    fixture.tool(ISOLATED_WRITE).await?;
    let report = fixture.ended_run().await?;
    assert!(
        report.contains("isolation: merged"),
        "tricky names must still merge: {report}"
    );
    let text = fixture.task_text()?;
    for name in &wanted {
        assert!(
            text.contains(name),
            "saved changed list names {name:?}: {text}"
        );
    }
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&root_text)).await;
    Ok(())
}

/// Builds the tricky-names repo and returns its saved diff texts.
fn newline_setup() -> (String, String, String, String, Vec<String>) {
    let root = unique_temp_root("dalgona-names");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp repo root");
    let base = seed_new_repo(&root);
    let expected = [
        "normal.txt",
        "with\nnewline.txt",
        "with\ttab.txt",
        "with\"quote.txt",
    ];
    for name in expected {
        std::fs::write(root.join(name), b"data").expect("tricky");
    }
    run_git(&root, &["add", "-A"]);
    let root_text = root.display().to_string();
    let (diff_text, names_text) = cached_texts(&root_text, &base);
    let wanted = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
    (root_text, base, diff_text, names_text, wanted)
}

#[tokio::test]
async fn a_sibling_change_outside_the_nested_workspace_is_retained_not_merged() -> TestResult {
    let setup = tokio::task::spawn_blocking(sibling_setup)
        .await
        .expect("setup joins");
    let (root_text, base, diff_text, names_text) = setup;
    check_sibling_escape(&root_text, &base, &diff_text, &names_text).await?;
    check_valid_subdir_merge(&root_text, &base).await?;
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&root_text)).await;
    Ok(())
}

/// Builds the nested repo with one in-scope and two escaping changes.
fn sibling_setup() -> (String, String, String, String) {
    let root = unique_temp_root("dalgona-nested");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("subdir")).expect("subdir");
    let base = seed_new_repo(&root);
    std::fs::write(root.join("subdir/good.txt"), b"good").expect("good");
    std::fs::write(root.join("sibling-bad.txt"), b"bad").expect("bad");
    std::fs::create_dir_all(root.join("sibling")).expect("sibling dir");
    std::fs::write(root.join("sibling/bad.txt"), b"bad").expect("sibling bad");
    run_git(&root, &["add", "-A"]);
    let root_text = root.display().to_string();
    let (diff_text, names_text) = cached_texts(&root_text, &base);
    (root_text, base, diff_text, names_text)
}

/// Runs the escaping change and proves the worktree is retained, not merged.
async fn check_sibling_escape(
    root_text: &str,
    base: &str,
    diff_text: &str,
    names_text: &str,
) -> TestResult {
    let mixed_workspace =
        dal_core::Workspace::new(std::path::PathBuf::from(format!("{root_text}/subdir")))
            .expect("absolute workspace");
    let mixed = Fixture::open_at(Runtime::new(isolation_config()?)?, None, mixed_workspace).await?;
    {
        let mut script = mixed.script();
        script.run_outputs.extend(merged_script_outputs(
            root_text, base, diff_text, names_text,
        ));
    }
    mixed.tool(ISOLATED_WRITE).await?;
    let mixed_report = mixed.ended_run().await?;
    assert!(
        !mixed_report.contains("isolation: merged"),
        "sibling escape must not merge: {mixed_report}"
    );
    assert!(
        mixed.task_artifact(ArtifactFile::RetainedJson).is_ok(),
        "sibling escape must retain the worktree with its recovery path: {mixed_report}"
    );
    Ok(())
}

/// Runs the in-scope subdir change and proves it still merges.
async fn check_valid_subdir_merge(root_text: &str, base: &str) -> TestResult {
    let valid_workspace =
        dal_core::Workspace::new(std::path::PathBuf::from(format!("{root_text}/subdir")))
            .expect("absolute workspace");
    let valid = Fixture::open_at(Runtime::new(isolation_config()?)?, None, valid_workspace).await?;
    {
        let mut script = valid.script();
        script.run_outputs.extend(merged_script_outputs(
            root_text,
            base,
            "diff --git a/subdir/good.txt b/subdir/good.txt\n",
            "subdir/good.txt\n",
        ));
    }
    valid.tool(ISOLATED_WRITE).await?;
    let valid_report = valid.ended_run().await?;
    assert!(
        valid_report.contains("isolation: merged"),
        "a valid subdir change must still merge: {valid_report}"
    );
    Ok(())
}

/// Builds the top and data roots of the timeout fixture with a seed commit.
fn timeout_dirs() -> (String, String) {
    let tag = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    );
    let top = std::env::temp_dir().join(format!("dalgona-timeout-top-{tag}"));
    let data = std::env::temp_dir().join(format!("dalgona-timeout-data-{tag}"));
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&top).expect("top root");
    std::fs::create_dir_all(&data).expect("data root");
    run_git(&top, &["init"]);
    run_git(&top, &["config", "user.email", "test@example.com"]);
    run_git(&top, &["config", "user.name", "test"]);
    run_git(
        &top,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "seed",
        ],
    );
    let top_text = top.display().to_string();
    let data_text = data.display().to_string();
    (top_text, data_text)
}

#[tokio::test(start_paused = true)]
async fn a_timeout_abort_retains_the_dirty_worktree() -> TestResult {
    let setup = tokio::task::spawn_blocking(timeout_dirs)
        .await
        .expect("setup joins");
    let (top_text, data_text) = setup;
    let mut config = isolation_config()?;
    config.data_root = Some(std::path::PathBuf::from(data_text.as_str()));
    let runtime = Runtime::new(config)?;
    let stage = Arc::new(ApplyGate::new());
    let host = Arc::new_cyclic(|this| Host {
        this: this.clone(),
        stage_gate: Some(Arc::clone(&stage)),
        passthrough_git: true,
        ..Host::default()
    });
    let session = SessionId::new_v7();
    *locked(&host.runtime) = Some((runtime.clone(), session));
    let services: Arc<dyn Services> = host.clone();
    let workspace = dal_core::Workspace::new(std::path::PathBuf::from(top_text.as_str()))
        .expect("absolute workspace");
    runtime
        .open(
            dal_core::ext::SessionStart {
                session,
                workspace,
                resumed: false,
            },
            HookCx::for_test(services, session, None),
        )
        .await?;
    let fixture = Fixture {
        runtime,
        host,
        session,
    };
    fixture.user_input().await?;
    fixture.tool(ISOLATED_WRITE).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), stage.entered(1))
        .await
        .expect("staging reached");
    let task = {
        let script = fixture.script();
        script
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_some())
            .ok_or("task absent")?
            .0
            .id
    };
    let worktree = std::path::PathBuf::from(data_text.as_str())
        .join("worktrees")
        .join(session.to_string())
        .join(task.to_string());
    let dirty = worktree.join("dirty.txt");
    tokio::fs::write(&dirty, b"dirty work").await?;
    let ((), ()) = tokio::join!(
        async {
            fixture
                .runtime
                .close(
                    fixture.session,
                    dal_core::ext::SessionEnd {
                        session: fixture.session,
                        reason: "close".into(),
                    },
                )
                .await;
        },
        async {
            tokio::time::advance(std::time::Duration::from_secs(5)).await;
        }
    );
    stage.release.add_permits(1);
    let retained: RetainedRecord =
        sonic_rs::from_slice(&fixture.task_artifact(ArtifactFile::RetainedJson)?)?;
    assert!(
        !retained.worktree.is_empty(),
        "timeout must surface its recovery path"
    );
    let kept = tokio::fs::try_exists(&dirty).await.unwrap_or(false)
        || tokio::fs::try_exists(std::path::Path::new(&retained.worktree).join("dirty.txt"))
            .await
            .unwrap_or(false);
    assert!(kept, "a timed-out run must never delete its dirty worktree");
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&top_text)).await;
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&data_text)).await;
    Ok(())
}
/// Builds the real-git symlink-scope fixture: a physical repository top, a
/// physical `subdir` inside it, and a symlink spelling of that subdirectory
/// outside the repository. Returns the physical texts and both spellings.
#[cfg(unix)]
fn symlink_scope_dirs() -> (String, String, String) {
    let root = unique_temp_root("dalgona-symlink");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("subdir")).expect("subdir");
    seed_new_repo(&root);
    std::fs::write(root.join("subdir/nested.txt"), b"nested").expect("nested file");
    run_git(&root, &["add", "-A"]);
    let link = unique_temp_root("dalgona-symlink-link");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(root.join("subdir"), &link).expect("subdir symlink");
    let top_text = root.display().to_string();
    let physical_text = root.join("subdir").display().to_string();
    let link_text = link.display().to_string();
    (top_text, physical_text, link_text)
}

/// The passthrough host of the timeout fixture: every `git` call the owner
/// makes runs the real git, so preflight and capture cross the real
/// filesystem instead of a scripted answer. The optional stage gate parks
/// the real `git add -A` of the capture until the test releases it.
#[cfg(unix)]
async fn passthrough_fixture(
    config: crate::orchestration::OrchestrationConfig,
    workspace: dal_core::Workspace,
    stage_gate: Option<Arc<ApplyGate>>,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let runtime = Runtime::new(config)?;
    let host = Arc::new_cyclic(|this| Host {
        this: this.clone(),
        stage_gate,
        passthrough_git: true,
        ..Host::default()
    });
    let session = SessionId::new_v7();
    *locked(&host.runtime) = Some((runtime.clone(), session));
    let services: Arc<dyn Services> = host.clone();
    runtime
        .open(
            dal_core::ext::SessionStart {
                session,
                workspace,
                resumed: false,
            },
            HookCx::for_test(services, session, None),
        )
        .await?;
    let fixture = Fixture {
        runtime,
        host,
        session,
    };
    fixture.user_input().await?;
    Ok(fixture)
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_subdir_workspace_keeps_the_child_in_its_subdirectory() -> TestResult {
    let setup = tokio::task::spawn_blocking(symlink_scope_dirs)
        .await
        .expect("setup joins");
    let (top_text, _physical_text, link_text) = setup;
    let data = unique_temp_root("dalgona-scope");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("data root");
    let mut config = isolation_config()?;
    config.data_root = Some(data.clone());
    let workspace = dal_core::Workspace::new(std::path::PathBuf::from(link_text.as_str()))
        .expect("absolute workspace");
    let fixture = passthrough_fixture(config, workspace, None).await?;
    fixture.tool(ISOLATED_WRITE).await?;
    let report = fixture.ended_run().await?;
    let (task, child_workspace) = {
        let script = fixture.script();
        let task = script
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_some())
            .ok_or("task absent")?
            .0
            .id;
        let started = script
            .starts
            .first()
            .map(|(_, start)| start.clone())
            .ok_or("the isolated child never started")?;
        (task, started.workspace)
    };
    let child_workspace = child_workspace.ok_or("the child was given no workspace")?;
    // The isolated worktree root plus the derived relative workspace must be
    // the assigned subdirectory: the child never receives the whole checkout,
    // which is exactly the pre-repair widening when the session's symlink
    // spelling failed to strip from the physical top.
    let worktree_root = data
        .join("worktrees")
        .join(fixture.session.to_string())
        .join(task.to_string());
    let expected = worktree_root.join("subdir");
    assert_eq!(
        child_workspace.as_path(),
        expected.as_path(),
        "the child workspace must stay the task subdirectory: {child_workspace:?} ({report})"
    );
    assert_ne!(
        child_workspace.as_path(),
        worktree_root.as_path(),
        "the child must never receive the worktree root: {child_workspace:?}"
    );
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&top_text)).await;
    let _ = tokio::fs::remove_dir_all(&data).await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_capture_failure_settles_the_task_job_with_the_kept_recovery_path() -> TestResult {
    let setup = tokio::task::spawn_blocking(symlink_scope_dirs)
        .await
        .expect("setup joins");
    let (top_text, _physical_text, _link_text) = setup;
    let data = unique_temp_root("dalgona-kept");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).expect("data root");
    let mut config = isolation_config()?;
    config.data_root = Some(data.clone());
    let workspace = dal_core::Workspace::new(std::path::PathBuf::from(top_text.as_str()))
        .expect("absolute workspace");
    let stage = Arc::new(ApplyGate::new());
    let fixture = passthrough_fixture(config, workspace, Some(Arc::clone(&stage))).await?;
    fixture.tool(ISOLATED_WRITE).await?;
    // Causal schedule: wait until the real capture `git add -A` is parked in
    // the stage gate. The task job and its worktree exist by then, so the id
    // is readable without racing the spawn.
    tokio::time::timeout(std::time::Duration::from_secs(5), stage.entered(1)).await?;
    let task = {
        let script = fixture.script();
        script
            .jobs
            .iter()
            .find(|(_, parent, _)| parent.is_some())
            .ok_or("task absent")?
            .0
            .id
    };
    let worktree = data
        .join("worktrees")
        .join(fixture.session.to_string())
        .join(task.to_string());
    // Detach the worktree from its repository so the parked, released
    // capture call fails: the tree must stay on disk.
    tokio::task::spawn_blocking({
        let admin = worktree.join(".git");
        move || std::fs::remove_file(&admin).expect("detach worktree admin")
    })
    .await
    .expect("detach joins");
    stage.release.add_permits(1);
    let report = fixture.ended_run().await?;
    let task_text = fixture.task_text()?;
    assert!(
        !report.contains("isolation: merged"),
        "a detached worktree cannot merge: {report}"
    );
    assert!(
        worktree.is_dir(),
        "a capture failure must keep the worktree on disk: {}",
        worktree.display()
    );
    let kept_text = worktree.display().to_string();
    assert!(
        task_text.contains(kept_text.as_str()),
        "the settled task job text must name the kept recovery directory: {task_text}"
    );
    assert!(
        task_text.contains(": failed"),
        "a capture failure must settle the task as failed: {task_text}"
    );
    assert!(
        matches!(
            fixture
                .script()
                .jobs
                .iter()
                .find(|(_, parent, _)| parent.is_some())
                .ok_or("task absent")?
                .0
                .state,
            JobStateView::Done(dal_core::JobOutcome::Failed { .. })
        ),
        "a capture failure must settle the task job as failed: {task_text}"
    );
    assert!(
        task_text.contains("Recover the tree at that path"),
        "the settled task job text must tell the user how to recover: {task_text}"
    );
    let _ = tokio::fs::remove_dir_all(std::path::Path::new(&top_text)).await;
    let _ = tokio::fs::remove_dir_all(&data).await;
    Ok(())
}

/// The stress-boundary proofs: the per-checkout merge probe at the real
/// acquisition/drop boundary and the owner mailbox under deterministic
/// hold-and-drain pressure. The scripted-host tests below run on every suite
/// pass; the 500-child real-git scale test is gated on `DALGONA_ORCH_STRESS`
/// like the gates.
#[expect(
    clippy::disallowed_methods,
    reason = "the probe test parks a waiter task behind the holder it measures"
)]
#[tokio::test]
async fn merge_lock_probe_counts_one_hold_per_checkout() {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::SeqCst;

    let locks = super::merge_locks::MergeLocks::default();
    let top = std::path::Path::new("/tmp/repo-top");
    let key = super::merge_locks::MergeLocks::canonical_key(top).await;
    let first = locks.hold(top).await;
    assert_eq!(locks.key_holds(&key), 1, "one merge holds the checkout");
    assert_eq!(locks.key_peak(&key), 1, "the peak opens at one");
    // A waiter queues behind the holder; it must not touch the probe until
    // it acquires.
    let entered = Arc::new(AtomicBool::new(false));
    let waiter = tokio::spawn({
        let locks = locks.clone();
        let entered = Arc::clone(&entered);
        async move {
            let _hold = locks.hold(top).await;
            entered.store(true, SeqCst);
        }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert!(
        !entered.load(SeqCst),
        "the waiter cannot acquire while the holder runs"
    );
    assert_eq!(locks.key_holds(&key), 1, "the queued waiter holds nothing");
    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
        .await
        .expect("the waiter acquires after release")
        .expect("the waiter joins");
    assert_eq!(locks.key_holds(&key), 0, "the waiter released its hold");
    assert_eq!(locks.key_peak(&key), 1, "serialized holds never overlap");
    assert_eq!(locks.len(), 0, "the registry forgets the slot");
    assert_eq!(locks.distinct_holds(), 0, "no checkout holds a merge");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the probe test aborts a parked holder task to prove RAII release"
)]
#[tokio::test]
async fn a_cancelled_merge_hold_returns_its_probe_count() {
    let locks = super::merge_locks::MergeLocks::default();
    let top = std::path::Path::new("/tmp/repo-top");
    let key = super::merge_locks::MergeLocks::canonical_key(top).await;
    let park = Arc::new(tokio::sync::Notify::new());
    let holder = tokio::spawn({
        let locks = locks.clone();
        let park = Arc::clone(&park);
        async move {
            let _hold = locks.hold(top).await;
            park.notified().await;
        }
    });
    // The holder acquired exactly one probe count before it parked.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while locks.key_holds(&key) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the holder acquires");
    assert_eq!(locks.key_peak(&key), 1);
    // Aborting the parked holder unwinds the RAII hold like any release.
    holder.abort();
    let _ = holder.await;
    assert_eq!(
        locks.key_holds(&key),
        0,
        "a cancelled merge carries its probe count back to zero"
    );
    // The lock itself is free again: the next merge acquires at once with
    // the peak intact.
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), locks.hold(top))
        .await
        .expect("the lock is free after the abort");
    assert_eq!(locks.key_holds(&key), 1);
    drop(next);
    assert_eq!(locks.key_holds(&key), 0);
    assert_eq!(locks.key_peak(&key), 1, "no two holds ever overlapped");
    assert_eq!(locks.len(), 0, "the registry forgets the slot");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the probe test aborts a queued waiter task to prove it never counts"
)]
#[tokio::test]
async fn a_queued_waiter_aborted_before_acquiring_leaks_no_count() {
    let locks = super::merge_locks::MergeLocks::default();
    let top = std::path::Path::new("/tmp/repo-top");
    let key = super::merge_locks::MergeLocks::canonical_key(top).await;
    let first = locks.hold(top).await;
    let waiter = tokio::spawn({
        let locks = locks.clone();
        async move {
            let _hold = locks.hold(top).await;
        }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    // The waiter is still queued: aborting it must not move the probe.
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(
        locks.key_holds(&key),
        1,
        "only the live holder counts while a waiter dies queued"
    );
    drop(first);
    assert_eq!(locks.key_holds(&key), 0);
    assert_eq!(locks.key_peak(&key), 1);
    assert_eq!(locks.len(), 0, "the registry forgets the slot");
}

#[tokio::test(start_paused = true)]
async fn same_workspace_merges_serialize_with_peak_one() -> TestResult {
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    let runtime = Runtime::new(config)?;
    let gate = Arc::new(ApplyGate::new());
    let first = Fixture::open_on(runtime.clone(), Some(Arc::clone(&gate))).await?;
    let second = Fixture::open_on(runtime.clone(), Some(Arc::clone(&gate))).await?;
    for fixture in [&first, &second] {
        let mut script = fixture.script();
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    first.tool(ISOLATED_WRITE).await?;
    second.tool(ISOLATED_WRITE).await?;
    let top =
        super::merge_locks::MergeLocks::canonical_key(std::path::Path::new(&temp_toplevel())).await;
    let window = std::time::Duration::from_secs(5);
    tokio::time::timeout(window, gate.entered(1)).await?;
    let Some(ApplyEvent::Enter(holder)) = gate.events().first().copied() else {
        return Err("no merge reached apply".into());
    };
    let waiter = if holder == first.session {
        second.session
    } else {
        first.session
    };
    // The parked apply sits inside its merge hold: exactly one checkout
    // counts one live hold while the sibling queues behind the gate.
    assert_eq!(
        runtime.merge_locks.key_holds(&top),
        1,
        "one workspace counts one live merge hold"
    );
    assert_eq!(runtime.merge_locks.key_peak(&top), 1);
    assert_eq!(runtime.merge_locks.distinct_holds(), 1);
    let overlapped = tokio::time::timeout(std::time::Duration::from_secs(1), gate.entered(2)).await;
    assert!(
        overlapped.is_err(),
        "the second merge reached apply while the first held the checkout"
    );
    gate.release.add_permits(1);
    tokio::time::timeout(window, gate.entered(2)).await?;
    gate.release.add_permits(1);
    let (first_report, second_report) = (first.ended_run().await?, second.ended_run().await?);
    assert!(first_report.contains("isolation: merged"), "{first_report}");
    assert!(
        second_report.contains("isolation: merged"),
        "{second_report}"
    );
    assert_eq!(
        gate.events(),
        [
            ApplyEvent::Enter(holder),
            ApplyEvent::Exit(holder),
            ApplyEvent::Enter(waiter),
            ApplyEvent::Exit(waiter),
        ]
    );
    assert_eq!(
        runtime.merge_locks.key_holds(&top),
        0,
        "both merges released their holds"
    );
    assert_eq!(
        runtime.merge_locks.key_peak(&top),
        1,
        "the two same-checkout merges never overlapped"
    );
    assert_eq!(runtime.merge_locks.distinct_holds(), 0);
    assert_eq!(runtime.merge_locks.len(), 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn distinct_workspaces_merge_in_parallel() -> TestResult {
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    let runtime = Runtime::new(config)?;
    let gate = Arc::new(ApplyGate::new());
    let root = std::env::temp_dir();
    tokio::fs::create_dir_all(root.join("alpha")).await?;
    tokio::fs::create_dir_all(root.join("beta")).await?;
    let alpha = dal_core::Workspace::new(root.join("alpha")).expect("absolute workspace");
    let beta = dal_core::Workspace::new(root.join("beta")).expect("absolute workspace");
    let top_alpha_text = root.join("alpha").display().to_string();
    let top_beta_text = root.join("beta").display().to_string();
    let first = Fixture::open_at(runtime.clone(), Some(Arc::clone(&gate)), alpha).await?;
    let second = Fixture::open_at(runtime.clone(), Some(Arc::clone(&gate)), beta).await?;
    {
        let mut script = first.script();
        script.run_outputs.extend(isolated_prefix_for_top(
            &top_alpha_text,
            "code\n",
            "diff --git a/code b/code\n",
        ));
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    {
        let mut script = second.script();
        script.run_outputs.extend(isolated_prefix_for_top(
            &top_beta_text,
            "code\n",
            "diff --git a/code b/code\n",
        ));
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    first.tool(ISOLATED_WRITE).await?;
    second.tool(ISOLATED_WRITE).await?;
    let top_alpha =
        super::merge_locks::MergeLocks::canonical_key(root.join("alpha").as_path()).await;
    let top_beta = super::merge_locks::MergeLocks::canonical_key(root.join("beta").as_path()).await;
    assert_ne!(top_alpha, top_beta, "the two checkouts are distinct keys");
    // Neither merge waits for the other: both applies park with no release,
    // which a global lock could never let happen.
    let window = std::time::Duration::from_secs(5);
    tokio::time::timeout(window, gate.entered(2))
        .await
        .map_err(|_| "the second checkout merge waited on the first")?;
    assert_eq!(
        runtime.merge_locks.key_holds(&top_alpha),
        1,
        "the first checkout holds its own merge"
    );
    assert_eq!(
        runtime.merge_locks.key_holds(&top_beta),
        1,
        "the second checkout holds its own merge"
    );
    assert_eq!(
        runtime.merge_locks.key_peak(&top_alpha),
        1,
        "one checkout never merges twice at once"
    );
    assert_eq!(
        runtime.merge_locks.key_peak(&top_beta),
        1,
        "one checkout never merges twice at once"
    );
    assert_eq!(
        runtime.merge_locks.distinct_holds(),
        2,
        "two unrelated checkouts hold two merges"
    );
    gate.release.add_permits(2);
    let (first_report, second_report) = (first.ended_run().await?, second.ended_run().await?);
    assert!(first_report.contains("isolation: merged"), "{first_report}");
    assert!(
        second_report.contains("isolation: merged"),
        "{second_report}"
    );
    assert_eq!(runtime.merge_locks.key_peak(&top_alpha), 1);
    assert_eq!(runtime.merge_locks.key_peak(&top_beta), 1);
    assert_eq!(runtime.merge_locks.distinct_holds(), 0);
    assert_eq!(runtime.merge_locks.len(), 0);
    let _ = tokio::fs::remove_dir_all(root.join("alpha")).await;
    let _ = tokio::fs::remove_dir_all(root.join("beta")).await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_failed_apply_check_releases_the_hold_for_the_next_merge() -> TestResult {
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    let runtime = Runtime::new(config)?;
    let gate = Arc::new(ApplyGate::new());
    let first = Fixture::open_on(runtime.clone(), None).await?;
    let second = Fixture::open_on(runtime.clone(), Some(Arc::clone(&gate))).await?;
    {
        // The checkout refuses the apply check, so the first merge retains
        // its worktree down the error path without ever reaching apply.
        let mut script = first.script();
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend([
            Ok(failed_output("error: patch failed: code:1")),
            Ok(output("")),
            Ok(output("")),
        ]);
    }
    {
        let mut script = second.script();
        script.run_outputs.extend(isolated_prefix());
        script.run_outputs.extend((0..4).map(|_| Ok(output(""))));
    }
    first.tool(ISOLATED_WRITE).await?;
    second.tool(ISOLATED_WRITE).await?;
    let top =
        super::merge_locks::MergeLocks::canonical_key(std::path::Path::new(&temp_toplevel())).await;
    let window = std::time::Duration::from_secs(5);
    // The second merge must reach apply: if the retained merge leaked its
    // hold, this waits out the window and the test fails loudly.
    tokio::time::timeout(window, gate.entered(1))
        .await
        .map_err(|_| "the retained merge kept the checkout lock")?;
    gate.release.add_permits(1);
    let (first_report, second_report) = (first.ended_run().await?, second.ended_run().await?);
    assert!(
        first_report.contains("isolation: retained"),
        "{first_report}"
    );
    assert!(
        !first_report.contains("isolation: merged"),
        "{first_report}"
    );
    assert!(
        second_report.contains("isolation: merged"),
        "{second_report}"
    );
    assert_eq!(
        first.plain_apply_calls(),
        0,
        "the refused check never applies"
    );
    assert_eq!(
        runtime.merge_locks.key_holds(&top),
        0,
        "the error path released its hold"
    );
    assert_eq!(
        runtime.merge_locks.key_peak(&top),
        1,
        "the failed merge never overlapped the next one"
    );
    assert_eq!(runtime.merge_locks.len(), 0);
    Ok(())
}

/// One `agents list` call through the owner: a read-only delivery the test
/// can flood without starting work.
const LIST_AGENTS: &str = r#"{"action":"list"}"#;

/// The parked-owner flood: the mailbox bound plus the backpressure tail the
/// producers queue behind the parked owner.
const FLOOD: usize = super::super::constants::BATTERY_MAILBOX + 144;

/// The miniature width: every child the scale test starts.
const CHILDREN: usize = 500;

/// The miniature writers: the worktree children merging one file each.
const WRITERS: usize = 250;

/// Sends one `agents list` tool call at `session`, for the producers that
/// flood a parked owner.
async fn list_agents_call(
    runtime: Runtime,
    session: SessionId,
    caller: Caller,
    call: CallId,
) -> Result<String, ServiceError> {
    let args = RawJson::parse(LIST_AGENTS)
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    runtime
        .tool(
            session,
            caller,
            call,
            "agents",
            args,
            CancellationToken::new(),
        )
        .await
}

#[expect(
    clippy::disallowed_methods,
    reason = "the pressure test floods the mailbox from spawned producer tasks"
)]
#[tokio::test]
async fn a_parked_owner_applies_backpressure_and_loses_no_delivery() -> TestResult {
    use super::super::constants::BATTERY_MAILBOX;

    let fixture = Fixture::open().await?;
    let gate = fixture.runtime.parking_gate();
    // Park the owner inside its next `jobs` list, then prove it parked
    // before any producer starts: every later send queues behind it.
    gate.arm();
    let parked_call = tokio::spawn(list_agents_call(
        fixture.runtime.clone(),
        fixture.session,
        fixture.caller(),
        CallId::new("park"),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while gate.parked() < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "the owner never parked at the jobs list")?;
    // Flood past capacity: the first BATTERY_MAILBOX sends queue and the
    // rest wait for space instead of dropping anything.
    let mut producers = Vec::with_capacity(FLOOD);
    for index in 0..FLOOD {
        producers.push(tokio::spawn(list_agents_call(
            fixture.runtime.clone(),
            fixture.session,
            fixture.caller(),
            CallId::new(format!("flood-{index}")),
        )));
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while fixture.runtime.mailbox_fill_now(fixture.session) != Some(BATTERY_MAILBOX) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| "the mailbox never filled to capacity behind the parked owner")?;
    assert!(
        gate.parked() >= 1,
        "the owner held its park while producers queued"
    );
    // Drain: every queued and every backpressured delivery lands exactly
    // once, and the mailbox empties with the owner still alive.
    gate.release();
    let mut replies = Vec::with_capacity(FLOOD + 1);
    replies.push(
        tokio::time::timeout(std::time::Duration::from_secs(60), parked_call)
            .await
            .map_err(|_| "the parked delivery never returned")?
            .map_err(|error| format!("the parked delivery failed: {error}"))?,
    );
    for producer in producers {
        replies.push(
            tokio::time::timeout(std::time::Duration::from_secs(60), producer)
                .await
                .map_err(|_| "a backpressured delivery never returned")?
                .map_err(|error| format!("a backpressured delivery failed: {error}"))?,
        );
    }
    assert_eq!(
        replies.len(),
        FLOOD + 1,
        "every sent delivery answered exactly once"
    );
    assert!(
        replies.iter().all(Result::is_ok),
        "no delivery failed or was dropped under pressure"
    );
    assert_eq!(
        fixture.runtime.mailbox_fill_now(fixture.session),
        Some(0),
        "the drain emptied the mailbox: nothing leaked"
    );
    let peak = fixture
        .runtime
        .mailbox_fill_peak(fixture.session)
        .ok_or("the owner is gone")?;
    assert!(
        peak <= BATTERY_MAILBOX,
        "the mailbox never held more than its bound: {peak}"
    );
    // The owner is still functional after the flood.
    fixture.tool(LIST_AGENTS).await?;
    Ok(())
}

/// Opens the real-git scale checkout, its data root, and the session owner
/// the 500-child run drives.
async fn stress_open_fixture() -> Result<
    (
        std::path::PathBuf,
        std::path::PathBuf,
        Fixture,
        Arc<AtomicUsize>,
    ),
    Box<dyn std::error::Error>,
> {
    let (root, data) = tokio::task::spawn_blocking(|| {
        let root = unique_temp_root("dalgona-stress");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("stress root");
        seed_new_repo(&root);
        let data = unique_temp_root("dalgona-stress-data");
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).expect("stress data root");
        (root, data)
    })
    .await
    .expect("setup joins");
    let mut config = isolation_config()?;
    config.goal.enabled = false;
    config.data_root = Some(data.clone());
    let runtime = Runtime::new(config)?;
    let stress_files = Arc::new(AtomicUsize::new(0));
    let host = Arc::new_cyclic(|this| Host {
        this: this.clone(),
        passthrough_git: true,
        stress_files: Some(Arc::clone(&stress_files)),
        ..Host::default()
    });
    let session = SessionId::new_v7();
    *locked(&host.runtime) = Some((runtime.clone(), session));
    let services: Arc<dyn Services> = host.clone();
    let workspace = dal_core::Workspace::new(root.clone()).expect("absolute workspace");
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
    let fixture = Fixture {
        runtime: runtime.clone(),
        host,
        session,
    };
    fixture.user_input().await?;
    Ok((root, data, fixture, stress_files))
}

/// The map/shared plus write/worktree fan-out over the 250 writer items.
fn stress_run_request() -> String {
    let items: Vec<String> = (0..WRITERS)
        .map(|index| format!("unit-{index:04}"))
        .collect();
    let quoted = items
        .iter()
        .map(|item| format!("\"{item}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"action\":\"run\",\"steps\":[{{\"name\":\"map\",\"prompt\":\"inspect {{{{item}}}}\",\"items\":[{quoted}],\"workers\":64,\"tools\":[\"read\"],\"isolation\":\"shared\"}},{{\"name\":\"write\",\"prompt\":\"write {{{{item}}}}\",\"items\":[{quoted}],\"workers\":64,\"tools\":[\"patch\"],\"isolation\":\"worktree\",\"after\":[\"map\"]}}]}}"
    )
}

/// Waits out the 600 s delivery bound the gates use for the 500-child run.
async fn stress_wait_report(fixture: &Fixture) -> Result<String, Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(600), async {
        loop {
            let changed = fixture.host.changed.notified();
            if let Some(text) = fixture
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
    .map_err(|_| "the 500-child run did not settle".into())
}

/// The merge, start, file, lock, and mailbox assertions of the scale run.
async fn stress_verify_merges(
    report: &str,
    fixture: &Fixture,
    stress_files: &Arc<AtomicUsize>,
    root: &std::path::Path,
) -> TestResult {
    let merged = report.matches("isolation: merged").count();
    assert_eq!(merged, WRITERS, "all 250 worktree writers merged: {report}");
    assert!(
        !report.contains("isolation: retained") && !report.contains("isolation: kept"),
        "no merge was retained or kept: {report}"
    );
    assert_eq!(
        fixture.script().starts.len(),
        CHILDREN,
        "all 500 subagents started"
    );
    assert_eq!(
        stress_files.load(std::sync::atomic::Ordering::SeqCst),
        WRITERS,
        "only the 250 worktree children wrote files"
    );
    let top = super::merge_locks::MergeLocks::canonical_key(root).await;
    assert_eq!(
        fixture.runtime.merge_locks.key_peak(&top),
        1,
        "the 250 same-checkout merges never overlapped"
    );
    assert_eq!(fixture.runtime.merge_locks.key_holds(&top), 0);
    assert_eq!(fixture.runtime.merge_locks.distinct_holds(), 0);
    assert_eq!(fixture.runtime.merge_locks.len(), 0);
    let peak = fixture
        .runtime
        .mailbox_fill_peak(fixture.session)
        .ok_or("the owner is gone")?;
    assert!(
        peak <= super::super::constants::BATTERY_MAILBOX,
        "the mailbox never held more than its bound: {peak}"
    );
    Ok(())
}

/// The checkout holds exactly the 250 merged files and only the main tree.
async fn stress_verify_checkout(root: &std::path::Path) -> TestResult {
    let mut landed = 0;
    let mut entries = tokio::fs::read_dir(root)
        .await
        .map_err(|error| format!("checkout unreadable: {error}"))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| format!("checkout unreadable: {error}"))?
    {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("unit-"))
        {
            landed += 1;
        }
    }
    assert_eq!(
        landed, WRITERS,
        "the 250 merged files landed in the checkout"
    );
    let list_root = root.to_path_buf();
    let list = tokio::task::spawn_blocking(move || run_git(&list_root, &["worktree", "list"]))
        .await
        .expect("worktree list joins");
    assert_eq!(
        String::from_utf8_lossy(&list.stdout).lines().count(),
        1,
        "only the main tree remains: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    Ok(())
}

/// Open-salvage proofs: the battery salvages only own-session worktree
/// directories whose task jobs the live table does not hold, or holds as
/// ended `lost`.
///
/// Opens its owner with a fresh data root, then closes that owner: the proofs
/// drive `salvage_open_worktrees` directly, and a live owner shares the
/// scripted host. Its 250 ms delivery tick lists jobs too, so it could take
/// a one-shot `jobs_list_error` meant for salvage (or emit its own delivery
/// notice) between the test arming the script and salvage asking. Every
/// directory below is created by the test, so no other test's leftovers can
/// interfere.
async fn salvage_open() -> Result<(Fixture, std::path::PathBuf), Box<dyn std::error::Error>> {
    let data = unique_temp_root("dalgona-salvage");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data)?;
    let mut config = parse_config(None)?;
    config.goal.enabled = false;
    config.data_root = Some(data.clone());
    let fixture = Fixture::open_owner(config).await?;
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
    Ok((fixture, data))
}

fn salvage_job(id: JobId, state: JobStateView) -> (JobStatus, Option<JobId>, String) {
    (
        JobStatus {
            id,
            label: "task".into(),
            state,
            log: None,
            last_activity_at: dal_core::Timestamp::now(),
        },
        None,
        String::new(),
    )
}

fn salvage_dir(data: &std::path::Path, session: SessionId, task: JobId) -> std::path::PathBuf {
    let dir = data
        .join("worktrees")
        .join(session.to_string())
        .join(task.to_string());
    std::fs::create_dir_all(&dir).expect("the salvage worktree dir exists");
    dir
}

/// The base commit the scripted salvage trees record in their receipts: a
/// full object name, the shape open salvage accepts.
const SALVAGE_BASE: &str = "0123456789abcdef0123456789abcdef01234567";

/// The battery's creation receipt: open salvage trusts only trees carrying
/// it, and takes their work against the base it records.
fn salvage_receipt(data: &std::path::Path, session: SessionId, task: JobId) {
    let receipt = data
        .join("isolation")
        .join(session.to_string())
        .join(task.to_string());
    std::fs::create_dir_all(&receipt).expect("the salvage receipt exists");
    std::fs::write(receipt.join("base.txt"), format!("{SALVAGE_BASE}\n"))
        .expect("the receipt records its base");
}

/// Drives the open-salvage path directly against the scripted host and
/// returns the workspace its git calls start from.
async fn run_salvage(fixture: &Fixture, data: &std::path::Path) -> dal_core::Workspace {
    let workspace = ToolCx::for_test(fixture.host.clone()).workspace().clone();
    run_salvage_in(fixture, data, &workspace).await;
    workspace
}

/// Drives open salvage from `workspace`, the checkout the owner opened over.
/// The real-git proofs pass their repo top: the scripted test workspace is the
/// platform temp dir, where a real `rev-parse --show-toplevel` finds no repo.
async fn run_salvage_in(
    fixture: &Fixture,
    data: &std::path::Path,
    workspace: &dal_core::Workspace,
) {
    let services: Arc<dyn Services> = fixture.host.clone();
    super::salvage_open_worktrees(
        services.as_ref(),
        &fixture.caller(),
        fixture.session,
        workspace.as_path(),
        data,
    )
    .await;
}

fn argv_words(requests: &[RunRequest]) -> Vec<Vec<String>> {
    requests
        .iter()
        .map(|request| {
            request
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        })
        .collect()
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "scripted salvage boundary proof with inline fixture scripting; kept whole so each failure message reads against its setup"
)]
async fn open_salvage_removes_only_unheld_lost_trees() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let lost = JobId::new_v7();
    // `vanished` has no job row: a crashed task leaves no ledger row, so
    // absence from the live table is the lost signal. The store lock already
    // excluded a second opener, so this tree is ours, not foreign.
    let vanished = JobId::new_v7();
    let live = JobId::new_v7();
    let done = JobId::new_v7();
    {
        let mut script = fixture.script();
        script.notices.clear();
        script.jobs.push(salvage_job(
            lost,
            JobStateView::Done(dal_core::JobOutcome::Lost),
        ));
        script.jobs.push(salvage_job(live, JobStateView::Running));
        script.jobs.push(salvage_job(
            done,
            JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
        ));
    }
    for task in [lost, vanished, live, done] {
        salvage_dir(&data, fixture.session, task);
    }
    let dir_text = |task: JobId| {
        data.join("worktrees")
            .join(fixture.session.to_string())
            .join(task.to_string())
            .display()
            .to_string()
    };
    // Salvage sorts by task id for deterministic argv; script the outputs
    // and the expectations in that same order.
    let mut salvaged = vec![lost, vanished];
    salvaged.sort_by_key(JobId::to_string);
    let top = "/tmp/dalgona-salvage-top";
    for task in &salvaged {
        salvage_receipt(&data, fixture.session, *task);
    }
    {
        let mut script = fixture.script();
        script.run_outputs.push_back(Ok(output(top)));
        let mut listing = String::new();
        for task in &salvaged {
            listing.push_str("worktree ");
            listing.push_str(&dir_text(*task));
            listing.push('\n');
        }
        listing.push_str("HEAD deadbeef\n");
        script.run_outputs.push_back(Ok(output(listing.as_str())));
        for (n, _) in salvaged.iter().enumerate() {
            // Porcelain v1 is `XY PATH`: an unstaged modification is ` M`.
            // A one-column `M file.txt` is not a line git emits, and salvage
            // refuses a malformed status read by design.
            script.run_outputs.push_back(Ok(output(" M file.txt\n")));
            script
                .run_outputs
                .push_back(Ok(output(format!("delta {n}").as_str())));
            script.run_outputs.push_back(Ok(output("file.txt\0")));
            script.run_outputs.push_back(Ok(output("")));
        }
        script.run_outputs.push_back(Ok(output("")));
    }
    let workspace = run_salvage(&fixture, &data).await;
    let script = fixture.script();
    let workspace_text = workspace.as_path().display().to_string();
    // Top and registration first, then status, net diff, names, and remove
    // per tree in sorted order, then one prune. No staging step runs, so
    // untracked bytes can never merge into the patch. Every argv starts
    // with git and carries its explicit -C root.
    let mut expected = vec![
        words(&[
            "git",
            "-C",
            workspace_text.as_str(),
            "rev-parse",
            "--show-toplevel",
        ]),
        words(&["git", "-C", top, "worktree", "list", "--porcelain"]),
    ];
    for task in &salvaged {
        let dir = dir_text(*task);
        expected.push(words(&[
            "git",
            "-C",
            dir.as_str(),
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
        ]));
        expected.push(words(&[
            "git",
            "-C",
            dir.as_str(),
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
            SALVAGE_BASE,
        ]));
        expected.push(words(&[
            "git",
            "-C",
            dir.as_str(),
            "diff",
            "--name-only",
            "-z",
            SALVAGE_BASE,
        ]));
        expected.push(words(&[
            "git",
            "-C",
            top,
            "worktree",
            "remove",
            "--force",
            dir.as_str(),
        ]));
    }
    expected.push(words(&["git", "-C", top, "worktree", "prune"]));
    assert_eq!(
        argv_words(&script.run_requests),
        expected,
        "salvage runs only the fixed git argv for the lost trees"
    );
    for (n, task) in salvaged.iter().enumerate() {
        let delta = format!("delta {n}");
        assert!(
            script.artifacts.iter().any(|(job, file, bytes)| {
                job == task
                    && *file == ArtifactFile::DeltaPatch
                    && bytes.as_slice() == delta.as_bytes()
            }),
            "the lost delta is persisted through the artifact operation"
        );
    }
    for task in &salvaged {
        let status = script
            .artifacts
            .iter()
            .find(|(job, file, _)| job == task && *file == ArtifactFile::StatusTxt)
            .map(|(_, _, bytes)| bytes.clone())
            .ok_or("the status record was persisted through the artifact owner")?;
        assert_eq!(
            status, b" M file.txt\n",
            "the status metadata stays distinct from the patch"
        );
    }
    // Each tree's patch and status are durable, in that order, before that
    // tree's removal; the one prune comes last.
    let mut expected_order = Vec::new();
    for _ in &salvaged {
        expected_order.extend(["artifact:delta_patch", "artifact:status_txt", "remove"]);
    }
    expected_order.push("prune");
    assert_eq!(
        script.salvage_order, expected_order,
        "status is durable through the artifact owner before each removal"
    );
    assert_eq!(script.notices.len(), 1, "salvage sends one summary notice");
    let notice = &script.notices[0];
    assert_eq!(notice.kind.to_string(), "orchestration.salvage");
    assert!(
        salvaged
            .iter()
            .all(|task| notice.text.contains(task.to_string().as_str()))
            && notice.text.contains("delta.patch")
            && notice.text.contains("was not changed")
            && !notice.text.contains("skipped"),
        "the notice names the kept patches and reports no merge: {}",
        notice.text
    );
    for task in [live, done] {
        let dir = data
            .join("worktrees")
            .join(fixture.session.to_string())
            .join(task.to_string());
        assert!(dir.is_dir(), "a live or done tree stays in place: {dir:?}");
    }
    assert!(
        !script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == "apply")),
        "salvage never merges"
    );
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Asserts the reopen salvaged exactly the lost tree: the fixed salvage git
/// argv, the durable delta and status records, the ordered salvage events,
/// the single summary notice, and no merge.
fn assert_reopened_salvage(
    script: &Script,
    workspace_text: &str,
    top: &str,
    dir_text: &str,
    vanished: &JobId,
) {
    assert_eq!(
        argv_words(&script.run_requests),
        vec![
            words(&["git", "-C", workspace_text, "rev-parse", "--show-toplevel"]),
            words(&["git", "-C", top, "worktree", "list", "--porcelain"]),
            words(&[
                "git",
                "-C",
                dir_text,
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored"
            ]),
            words(&[
                "git",
                "-C",
                dir_text,
                "diff",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                SALVAGE_BASE
            ]),
            words(&[
                "git",
                "-C",
                dir_text,
                "diff",
                "--name-only",
                "-z",
                SALVAGE_BASE
            ]),
            words(&["git", "-C", top, "worktree", "remove", "--force", dir_text]),
            words(&["git", "-C", top, "worktree", "prune"]),
        ],
        "the reopen runs only the fixed salvage git argv for the lost tree"
    );
    assert!(
        script.artifacts.iter().any(|(job, file, bytes)| {
            job == vanished && *file == ArtifactFile::DeltaPatch && bytes.as_slice() == b"delta"
        }),
        "the reopened session persists the lost delta through the artifact owner"
    );
    assert!(
        script.artifacts.iter().any(|(job, file, bytes)| {
            job == vanished
                && *file == ArtifactFile::StatusTxt
                && bytes.as_slice() == b" M file.txt\n"
        }),
        "the reopened session persists the status record through the artifact owner"
    );
    assert_eq!(
        script.salvage_order,
        [
            "artifact:delta_patch",
            "artifact:status_txt",
            "remove",
            "prune"
        ],
        "status is durable before the removal on the reopen path"
    );
    assert_eq!(
        script.notices.len(),
        1,
        "the reopen sends one summary notice"
    );
    let notice = &script.notices[0];
    assert_eq!(notice.kind.to_string(), "orchestration.salvage");
    assert!(
        notice.text.contains(vanished.to_string().as_str())
            && notice.text.contains("delta.patch")
            && notice.text.contains("not changed")
            && !notice.text.contains("skipped"),
        "the notice names the kept patch and reports no merge: {}",
        notice.text
    );
    assert!(
        !script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == "apply")),
        "the reopen never merges"
    );
}

/// `Runtime::open` itself enters the salvage hook: reopening a session over a
/// data root that holds a leftover lost tree salvages it through the real
/// `salvage_open_worktrees` path — the delta is durable through the artifact
/// owner, one summary notice names the kept patch, and nothing merges. The
/// ambient salvage tests drive `salvage_open_worktrees` directly; this proves
/// the crash/reopen wiring the session-open path owns.
#[tokio::test]
async fn reopening_a_session_salvages_its_lost_worktree() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let top = "/tmp/dalgona-salvage-top";
    // A fresh session on the same runtime: the crashed task left no job row,
    // so its tree is lost, and the store lock already excluded a second
    // opener, so this tree is ours, not foreign.
    let session = SessionId::new_v7();
    let vanished = JobId::new_v7();
    let dir = salvage_dir(&data, session, vanished);
    salvage_receipt(&data, session, vanished);
    let dir_text = dir.display().to_string();
    {
        let mut script = fixture.script();
        script.jobs.clear();
        script.notices.clear();
        script_status_tree(&mut script, top, &dir_text);
        script_removal_and_prune(&mut script);
    }
    let services: Arc<dyn Services> = fixture.host.clone();
    let workspace = ToolCx::for_test(fixture.host.clone()).workspace().clone();
    let workspace_text = workspace.as_path().display().to_string();
    let cx = HookCx::for_test(services, session, None);
    fixture
        .runtime
        .open(
            SessionStart {
                session,
                workspace,
                resumed: true,
            },
            cx,
        )
        .await?;
    {
        let script = fixture.script();
        assert_reopened_salvage(&script, &workspace_text, top, &dir_text, &vanished);
    }
    fixture
        .runtime
        .close(
            session,
            SessionEnd {
                session,
                reason: "close".into(),
            },
        )
        .await;
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Scripts the git answers for one salvageable lost tree up to its removal:
/// checkout top, worktree registration, status, net diff, and changed names.
fn script_status_tree(script: &mut Script, top: &str, dir_text: &str) {
    script.run_outputs.push_back(Ok(output(top)));
    script.run_outputs.push_back(Ok(output(
        format!("worktree {dir_text}\nHEAD deadbeef\n").as_str(),
    )));
    script.run_outputs.push_back(Ok(output(" M file.txt\n")));
    script.run_outputs.push_back(Ok(output("delta")));
    script.run_outputs.push_back(Ok(output("file.txt\0")));
}

/// Scripts the answers to the one worktree remove and the one prune.
fn script_removal_and_prune(script: &mut Script) {
    script.run_outputs.push_back(Ok(output("")));
    script.run_outputs.push_back(Ok(output("")));
}

/// The status record is part of the salvage: when only it fails to persist,
/// the patch is already durable but the tree must stay, with no removal and
/// no prune. Once the store recovers, the same tree salvages and is removed
/// exactly once, after both records are durable.
#[tokio::test]
async fn open_salvage_status_failure_keeps_tree_until_status_is_durable() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let task = JobId::new_v7();
    let dir = salvage_dir(&data, fixture.session, task);
    salvage_receipt(&data, fixture.session, task);
    let dir_text = dir.display().to_string();
    let top = "/tmp/dalgona-salvage-status-top";
    {
        let mut script = fixture.script();
        script.notices.clear();
        script_status_tree(&mut script, top, &dir_text);
        script.artifact_fail_file = Some((ArtifactFile::StatusTxt, "the status store is down"));
    }
    run_salvage(&fixture, &data).await;
    {
        let script = fixture.script();
        assert!(
            !script.run_requests.iter().any(|request| request
                .argv
                .iter()
                .any(|word| word == "remove" || word == "prune")),
            "a tree without a durable status record is neither removed nor pruned: {:?}",
            argv_words(&script.run_requests)
        );
        assert_eq!(
            script.salvage_order,
            ["artifact:delta_patch"],
            "the patch persisted, the status did not, and nothing followed"
        );
        assert!(
            script
                .artifacts
                .iter()
                .all(|(_, file, _)| *file != ArtifactFile::StatusTxt),
            "the failed status record is not recorded"
        );
        assert!(dir.is_dir(), "the tree stays in place: {dir:?}");
        assert_eq!(script.notices.len(), 1, "salvage sends one summary notice");
        let text = &script.notices[0].text;
        assert!(
            text.contains(task.to_string().as_str())
                && text.contains("the status record could not be saved")
                && text.contains("prune skipped")
                && !text.contains("kept at"),
            "the notice names the kept tree and the failed status record: {text}"
        );
    }
    {
        let mut script = fixture.script();
        script.notices.clear();
        script.run_requests.clear();
        script.salvage_order.clear();
        script.artifacts.clear();
        script_status_tree(&mut script, top, &dir_text);
        script_removal_and_prune(&mut script);
    }
    run_salvage(&fixture, &data).await;
    {
        let script = fixture.script();
        assert_eq!(
            script.salvage_order,
            [
                "artifact:delta_patch",
                "artifact:status_txt",
                "remove",
                "prune"
            ],
            "with a healthy store both records are durable before the one removal"
        );
        let (_, _, status) = script
            .artifacts
            .iter()
            .find(|(job, file, _)| *job == task && *file == ArtifactFile::StatusTxt)
            .ok_or("the retry persisted the status record")?;
        assert_eq!(status.as_slice(), b" M file.txt\n");
        assert_eq!(script.notices.len(), 1);
        assert!(
            script.notices[0].text.contains("kept at"),
            "the retry reports the salvage: {}",
            script.notices[0].text
        );
    }
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

#[tokio::test]
async fn open_salvage_excludes_live_and_done_trees() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let mut tasks = Vec::new();
    {
        let mut script = fixture.script();
        script.notices.clear();
        for state in [
            JobStateView::Running,
            JobStateView::Detached,
            JobStateView::Done(dal_core::JobOutcome::Exited { code: 0 }),
            JobStateView::Done(dal_core::JobOutcome::Cancelled),
        ] {
            let task = JobId::new_v7();
            script.jobs.push(salvage_job(task, state));
            salvage_dir(&data, fixture.session, task);
            tasks.push(task);
        }
    }
    run_salvage(&fixture, &data).await;
    let script = fixture.script();
    assert!(
        script.run_requests.is_empty(),
        "excluded trees order no git: {:?}",
        argv_words(&script.run_requests)
    );
    assert!(
        script.artifacts.is_empty(),
        "excluded trees persist nothing"
    );
    assert!(script.notices.is_empty(), "excluded trees report nothing");
    for task in &tasks {
        let dir = data
            .join("worktrees")
            .join(fixture.session.to_string())
            .join(task.to_string());
        assert!(dir.is_dir(), "an excluded tree stays in place: {dir:?}");
    }
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

#[tokio::test]
async fn open_salvage_keeps_the_tree_when_its_delta_cannot_be_read() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let lost = JobId::new_v7();
    let lost_dir = salvage_dir(&data, fixture.session, lost);
    salvage_receipt(&data, fixture.session, lost);
    let lost_text = lost_dir.display().to_string();
    {
        let mut script = fixture.script();
        script.notices.clear();
        script.jobs.push(salvage_job(
            lost,
            JobStateView::Done(dal_core::JobOutcome::Lost),
        ));
        script
            .run_outputs
            .push_back(Ok(output("/tmp/dalgona-salvage-top")));
        script
            .run_outputs
            .push_back(Ok(output(format!("worktree {lost_text}\n").as_str())));
        script.run_outputs.push_back(Ok(output(" M f\n")));
        script
            .run_outputs
            .push_back(Ok(failed_output("fatal: not a git repository")));
    }
    run_salvage(&fixture, &data).await;
    let script = fixture.script();
    assert_eq!(
        script.run_requests.len(),
        4,
        "a failed net diff stops before remove and prune: {:?}",
        argv_words(&script.run_requests)
    );
    assert!(
        script.artifacts.is_empty(),
        "no delta means no patch artifact"
    );
    assert_eq!(script.notices.len(), 1);
    assert!(
        script.notices[0]
            .text
            .contains("the changes could not be computed"),
        "the notice names why the tree stayed: {}",
        script.notices[0].text
    );
    assert!(
        lost_dir.is_dir(),
        "the unreadable tree stays in place: {lost_dir:?}"
    );
    assert!(
        !script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == "apply")),
        "salvage never merges"
    );
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

#[tokio::test]
async fn open_salvage_touches_nothing_when_the_job_list_fails() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let lost = JobId::new_v7();
    {
        let mut script = fixture.script();
        script.notices.clear();
        script.jobs.push(salvage_job(
            lost,
            JobStateView::Done(dal_core::JobOutcome::Lost),
        ));
        script.jobs_list_error = Some("the list failed");
    }
    let lost_dir = salvage_dir(&data, fixture.session, lost);
    run_salvage(&fixture, &data).await;
    let script = fixture.script();
    assert!(
        script.jobs_list_error.is_none(),
        "salvage itself asked for the live table and took the failure"
    );
    assert_eq!(
        script.run_requests,
        [] as [RunRequest; 0],
        "without the live table salvage orders no git"
    );
    assert_eq!(script.artifacts, [] as [(JobId, ArtifactFile, Vec<u8>); 0]);
    assert_eq!(script.notices, [] as [Notice; 0]);
    assert!(lost_dir.is_dir(), "the tree stays in place: {lost_dir:?}");
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Runs one tool call through the real `tool_call` hook and returns its
/// verdict.
async fn tool_verdict(
    fixture: &Fixture,
    tool: &str,
    args: &str,
    id: &str,
) -> Result<dal_core::ext::ToolCallVerdict, Box<dyn std::error::Error>> {
    use dal_agent::ext::Hook as _;
    let turn = dal_core::TurnId::new(std::num::NonZeroU64::MIN);
    let verdict = super::ToolCallHook(fixture.runtime.clone())
        .call(
            dal_core::ext::ToolCallEvent {
                turn,
                call: CallId::new(id),
                tool: dal_core::Name::parse(tool)?,
                class: dal_core::ToolClass::Exec {
                    read_only: false,
                    grant: None,
                },
                args: RawJson::parse(args)?,
            },
            HookCx::for_test(fixture.host.clone(), fixture.session, Some(turn)),
        )
        .await?;
    Ok(verdict)
}

/// The hook rewrites only the one shape the manual promises: an `exec` call
/// whose command is a recognized sleep wait of ten seconds or more. Its
/// foreground window becomes five seconds and every other member, the command
/// above all, survives byte for byte. Every other call is allowed untouched.
#[tokio::test]
async fn only_a_classified_exec_sleep_wait_is_rewritten() -> TestResult {
    use dal_core::ext::ToolCallVerdict;
    use sonic_rs::JsonValueTrait as _;
    let fixture = Fixture::open().await?;
    let args = match tool_verdict(&fixture, "exec", r#"{"command":"sleep 30"}"#, "wait").await? {
        ToolCallVerdict::Rewrite { args } => args,
        other => return Err(format!("a sleep wait was not rewritten: {other:?}").into()),
    };
    let rewritten: sonic_rs::Value = sonic_rs::from_str(args.as_str())?;
    assert_eq!(
        rewritten
            .get("foreground_s")
            .and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(5),
        "the foreground window is bounded to five seconds: {}",
        args.as_str()
    );
    assert_eq!(
        rewritten.get("command").and_then(|value| value.as_str()),
        Some("sleep 30"),
        "the command itself is untouched: {}",
        args.as_str()
    );
    let untouched = [
        ("exec", r#"{"command":"echo done"}"#, "not a sleep wait"),
        (
            "exec",
            r#"{"command":"sleep 3"}"#,
            "a sleep under ten seconds",
        ),
        (
            "exec",
            r#"{"command":"sleep 30","foreground_s":2}"#,
            "a window already shorter than five seconds",
        ),
        (
            "read",
            r#"{"command":"sleep 30"}"#,
            "another tool carrying the same text",
        ),
    ];
    for (index, (tool, args, why)) in untouched.into_iter().enumerate() {
        let verdict = tool_verdict(&fixture, tool, args, &format!("other-{index}")).await?;
        assert!(
            matches!(verdict, ToolCallVerdict::Allow),
            "{why} must be allowed unchanged: {verdict:?}"
        );
    }
    Ok(())
}

/// With the sleep rewrite switched off the same call is allowed untouched.
#[tokio::test]
async fn a_disabled_sleep_rewrite_leaves_the_sleep_wait_alone() -> TestResult {
    let mut config = parse_config(None)?;
    config.sleep.enabled = false;
    let fixture = Fixture::open_config(config).await?;
    let verdict = tool_verdict(&fixture, "exec", r#"{"command":"sleep 30"}"#, "wait").await?;
    assert!(
        matches!(verdict, dal_core::ext::ToolCallVerdict::Allow),
        "{verdict:?}"
    );
    Ok(())
}

/// A repeated sleep wait is first rewritten and then stopped by the loop
/// guard: once the guard blocks, the call is blocked, never rewritten again.
#[tokio::test]
async fn the_loop_guard_blocks_a_repeated_sleep_wait_instead_of_rewriting_it() -> TestResult {
    use dal_core::ext::ToolCallVerdict;
    let fixture = Fixture::open().await?;
    let mut blocked_at = None;
    for index in 0..9 {
        let verdict = tool_verdict(
            &fixture,
            "exec",
            r#"{"command":"sleep 30"}"#,
            &format!("repeat-{index}"),
        )
        .await?;
        let rewritten = matches!(verdict, ToolCallVerdict::Rewrite { .. });
        let blocked = matches!(verdict, ToolCallVerdict::Block { .. });
        assert!(
            rewritten || blocked,
            "a classified sleep wait is rewritten or blocked: {verdict:?}"
        );
        if blocked {
            blocked_at.get_or_insert(index);
        } else {
            assert!(
                blocked_at.is_none(),
                "call {index} was rewritten after the guard had blocked"
            );
        }
    }
    assert!(
        blocked_at.is_some_and(|index| index > 0),
        "the guard blocked the repeat only after the first call passed: {blocked_at:?}"
    );
    Ok(())
}

/// The host's held set is durable and outlives the held job's own row, so a
/// held task has no row after a restart yet its tree must stay. Salvage
/// reads the set without changing it, skips the held tree before any git
/// call, and still salvages an unheld lost sibling of the same sweep.
#[tokio::test]
async fn open_salvage_leaves_a_tree_in_the_durable_held_set() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let top = "/tmp/dalgona-salvage-top";
    let held = JobId::new_v7();
    let lost = JobId::new_v7();
    let held_dir = salvage_dir(&data, fixture.session, held);
    let lost_dir = salvage_dir(&data, fixture.session, lost);
    for task in [held, lost] {
        salvage_receipt(&data, fixture.session, task);
    }
    let held_text = held_dir.display().to_string();
    let lost_text = lost_dir.display().to_string();
    {
        let mut script = fixture.script();
        script.jobs.clear();
        script.notices.clear();
        script.held = vec![held];
        script.run_outputs.push_back(Ok(output(top)));
        script.run_outputs.push_back(Ok(output(
            format!("worktree {held_text}\nworktree {lost_text}\n").as_str(),
        )));
        script.run_outputs.push_back(Ok(output(" M f.txt\n")));
        script.run_outputs.push_back(Ok(output("lost delta")));
        script.run_outputs.push_back(Ok(output("f.txt\0")));
        script.run_outputs.push_back(Ok(output("")));
        script.run_outputs.push_back(Ok(output("")));
    }
    run_salvage(&fixture, &data).await;
    let script = fixture.script();
    assert!(
        !script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == held_text.as_str())),
        "no git call names the held tree"
    );
    assert!(
        script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == lost_text.as_str())),
        "the unheld lost sibling is still salvaged"
    );
    assert!(
        script.artifacts.iter().all(|(job, _, _)| *job == lost),
        "nothing is saved for the held tree"
    );
    assert!(held_dir.is_dir(), "the held tree stays in place");
    assert_eq!(
        script.held,
        [held],
        "reading the held set changed nothing in it"
    );
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A held set that cannot be read is no held set: salvage orders no git,
/// saves nothing, and leaves every tree, rather than guess who is held.
#[tokio::test]
async fn open_salvage_touches_nothing_when_the_held_set_cannot_be_read() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let lost = JobId::new_v7();
    let lost_dir = salvage_dir(&data, fixture.session, lost);
    salvage_receipt(&data, fixture.session, lost);
    {
        let mut script = fixture.script();
        script.jobs.clear();
        script.notices.clear();
        script.held_error = Some("the held set is unreadable");
    }
    run_salvage(&fixture, &data).await;
    let script = fixture.script();
    assert!(
        script.held_error.is_none(),
        "salvage itself asked for the held set and took the failure"
    );
    assert_eq!(
        script.run_requests,
        [] as [RunRequest; 0],
        "without the held set salvage orders no git"
    );
    assert_eq!(script.artifacts, [] as [(JobId, ArtifactFile, Vec<u8>); 0]);
    assert_eq!(script.notices, [] as [Notice; 0]);
    assert!(lost_dir.is_dir(), "the tree stays in place: {lost_dir:?}");
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A base record that is missing, option-shaped, truncated, or not newline
/// terminated is never passed to git: salvage leaves the tree in place and
/// orders no status, diff, or removal for it.
#[tokio::test]
async fn open_salvage_leaves_trees_whose_receipt_names_no_valid_base() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let top = "/tmp/dalgona-salvage-top";
    let cases: [(&str, Option<&str>); 4] = [
        ("missing", None),
        ("option-shaped", Some("--output=/tmp/salvage-pwned\n")),
        (
            "truncated",
            Some("0123456789abcdef0123456789abcdef0123456\n"),
        ),
        ("unterminated", Some(SALVAGE_BASE)),
    ];
    let mut dirs = Vec::new();
    let mut listing = String::new();
    for (_, record) in cases {
        let task = JobId::new_v7();
        let dir = salvage_dir(&data, fixture.session, task);
        salvage_receipt(&data, fixture.session, task);
        let base = data
            .join("isolation")
            .join(fixture.session.to_string())
            .join(task.to_string())
            .join("base.txt");
        match record {
            None => std::fs::remove_file(&base)?,
            Some(text) => std::fs::write(&base, text)?,
        }
        listing.push_str("worktree ");
        listing.push_str(&dir.display().to_string());
        listing.push_str("\nHEAD deadbeef\n\n");
        dirs.push((task, dir));
    }
    {
        let mut script = fixture.script();
        script.jobs.clear();
        script.notices.clear();
        script.run_outputs.push_back(Ok(output(top)));
        script.run_outputs.push_back(Ok(output(listing.as_str())));
    }
    let workspace = run_salvage(&fixture, &data).await;
    let script = fixture.script();
    let workspace_text = workspace.as_path().display().to_string();
    assert_eq!(
        argv_words(&script.run_requests),
        vec![
            words(&[
                "git",
                "-C",
                workspace_text.as_str(),
                "rev-parse",
                "--show-toplevel"
            ]),
            words(&["git", "-C", top, "worktree", "list", "--porcelain"]),
        ],
        "no status, diff, or removal is ordered for a tree without a valid base"
    );
    assert!(script.artifacts.is_empty(), "nothing is captured or saved");
    assert_eq!(script.notices.len(), 1, "salvage sends one summary notice");
    let text = &script.notices[0].text;
    assert_eq!(
        text.matches("names no valid base commit").count(),
        dirs.len(),
        "the notice names every tree and why it stayed: {text}"
    );
    assert!(
        !text.contains("kept at"),
        "no tree is reported as kept: {text}"
    );
    for (task, dir) in &dirs {
        assert!(text.contains(task.to_string().as_str()), "{text}");
        assert!(dir.is_dir(), "the tree stays in place: {dir:?}");
    }
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Asserts only the unmarked tree was read, captured, and removed: the fixed
/// salvage argv names the plain tree, and no prune runs beside a left tree.
fn assert_only_plain_tree_salvaged(
    script: &Script,
    workspace_text: &str,
    top: &str,
    plain_text: &str,
) {
    assert_eq!(
        argv_words(&script.run_requests),
        vec![
            words(&["git", "-C", workspace_text, "rev-parse", "--show-toplevel"]),
            words(&["git", "-C", top, "worktree", "list", "--porcelain"]),
            words(&[
                "git",
                "-C",
                plain_text,
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored"
            ]),
            words(&[
                "git",
                "-C",
                plain_text,
                "diff",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                SALVAGE_BASE
            ]),
            words(&[
                "git",
                "-C",
                plain_text,
                "diff",
                "--name-only",
                "-z",
                SALVAGE_BASE
            ]),
            words(&[
                "git", "-C", top, "worktree", "remove", "--force", plain_text
            ]),
        ],
        "only the unmarked tree is read, captured, and removed; no prune runs beside a left tree"
    );
}

/// A tree marked by `retained.json` is left for hand recovery: an aborted run
/// keeps its trees while a parked writer may still use them. The mark skips
/// only that tree; an unmarked lost tree of the same sweep still salvages.
#[tokio::test]
async fn open_salvage_leaves_a_tree_marked_by_a_retained_record() -> TestResult {
    let (fixture, data) = salvage_open().await?;
    let top = "/tmp/dalgona-salvage-top";
    let marked = JobId::new_v7();
    let plain = JobId::new_v7();
    let marked_dir = salvage_dir(&data, fixture.session, marked);
    let plain_dir = salvage_dir(&data, fixture.session, plain);
    for task in [marked, plain] {
        salvage_receipt(&data, fixture.session, task);
    }
    std::fs::write(
        data.join("isolation")
            .join(fixture.session.to_string())
            .join(marked.to_string())
            .join("retained.json"),
        b"{}\n",
    )?;
    let marked_text = marked_dir.display().to_string();
    let plain_text = plain_dir.display().to_string();
    {
        let mut script = fixture.script();
        script.jobs.clear();
        script.notices.clear();
        script.run_outputs.push_back(Ok(output(top)));
        script.run_outputs.push_back(Ok(output(
            format!("worktree {marked_text}\nworktree {plain_text}\n").as_str(),
        )));
        script.run_outputs.push_back(Ok(output(" M f.txt\n")));
        script.run_outputs.push_back(Ok(output("plain delta")));
        script.run_outputs.push_back(Ok(output("f.txt\0")));
        script.run_outputs.push_back(Ok(output("")));
    }
    let workspace = run_salvage(&fixture, &data).await;
    let script = fixture.script();
    let workspace_text = workspace.as_path().display().to_string();
    assert_only_plain_tree_salvaged(&script, &workspace_text, top, &plain_text);
    assert!(
        !script
            .run_requests
            .iter()
            .any(|request| request.argv.iter().any(|word| word == marked_text.as_str())),
        "no git call names the marked tree"
    );
    assert!(
        script.artifacts.iter().all(|(job, _, _)| *job == plain),
        "nothing is saved for the marked tree"
    );
    assert!(marked_dir.is_dir(), "the marked tree stays in place");
    assert_eq!(script.notices.len(), 1, "salvage sends one summary notice");
    let text = &script.notices[0].text;
    assert!(
        text.contains(marked.to_string().as_str())
            && text.contains("retained record")
            && text.contains(plain.to_string().as_str())
            && text.contains("kept at"),
        "the notice names the marked tree and the kept patch: {text}"
    );
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Creation records the receipt: the run's base commit in `base.txt`, inside
/// private directories the way the artifact owner makes them. Open salvage
/// reads exactly this record, so a creation that wrote less would leave every
/// tree it makes unsalvageable.
#[tokio::test]
async fn a_created_worktree_records_the_run_base_in_its_creation_receipt() -> TestResult {
    let data = unique_temp_root("dalgona-receipt");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data)?;
    let mut config = isolation_config()?;
    config.data_root = Some(data.clone());
    let fixture = Fixture::open_config(config).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output(&temp_toplevel())),
        Ok(output("head")),
        Ok(output(SALVAGE_BASE)),
        Ok(output("")),
        Ok(output("")),
        Ok(output("diff --git a/code b/code\n")),
        Ok(output("code\n")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
        Ok(output("")),
    ]);
    fixture
        .tool(r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#)
        .await?;
    let report = fixture.ended_run().await?;
    assert!(report.contains("merged"), "{report}");
    let task = fixture
        .script()
        .jobs
        .iter()
        .find(|(_, parent, _)| parent.is_some())
        .ok_or("task absent")?
        .0
        .id;
    let isolation = data.join("isolation");
    let session_dir = isolation.join(fixture.session.to_string());
    let receipt = session_dir.join(task.to_string());
    assert_eq!(
        std::fs::read_to_string(receipt.join("base.txt"))?,
        format!("{SALVAGE_BASE}\n"),
        "the receipt records the base the worktree was created at"
    );
    assert_eq!(
        super::receipt_base(&receipt).await.as_deref(),
        Some(SALVAGE_BASE),
        "what creation writes is exactly what open salvage reads back"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for private in [
            &isolation,
            &session_dir,
            &receipt,
            &receipt.join("base.txt"),
        ] {
            let mode = std::fs::metadata(private)?.permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "{private:?} is closed to group and others: {mode:o}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A receipt that cannot be recorded fails the task before any child starts
/// and takes the fresh, empty worktree with it, so no registered tree is
/// left that salvage could never trust.
#[tokio::test]
async fn a_receipt_that_cannot_be_recorded_fails_the_task_and_removes_its_fresh_worktree()
-> TestResult {
    let data = unique_temp_root("dalgona-receipt-refused");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data)?;
    // A file where the isolation directory belongs: no receipt can be made.
    std::fs::write(data.join("isolation"), b"not a directory")?;
    let mut config = isolation_config()?;
    config.data_root = Some(data.clone());
    let fixture = Fixture::open_config(config).await?;
    fixture.script().run_outputs.extend([
        Ok(output("git version 2.47.0")),
        Ok(output(&temp_toplevel())),
        Ok(output("head")),
        Ok(output("snapshot")),
        Ok(output("")),
        Ok(output("")),
    ]);
    fixture
        .tool(r#"{"action":"run","steps":[{"name":"write","prompt":"write","tools":["patch"],"isolation":"worktree"}]}"#)
        .await?;
    let report = fixture.ended_run().await?;
    assert!(
        report.contains("the isolation receipt could not be recorded"),
        "the run names why the task failed: {report}"
    );
    let script = fixture.script();
    assert!(
        script.starts.is_empty(),
        "no child starts in a tree salvage could not trust"
    );
    let removals: Vec<Vec<String>> = argv_words(&script.run_requests)
        .into_iter()
        .filter(|argv| argv.iter().any(|word| word == "remove"))
        .collect();
    let added: Vec<Vec<String>> = argv_words(&script.run_requests)
        .into_iter()
        .filter(|argv| argv.iter().any(|word| word == "add"))
        .collect();
    assert_eq!(added.len(), 1, "one worktree was created: {added:?}");
    assert_eq!(removals.len(), 1, "and that one was removed: {removals:?}");
    assert_eq!(
        removals[0].last(),
        added[0].get(6),
        "the removal names the worktree the failed creation made"
    );
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// The status read keeps only what no diff against the base can carry:
/// untracked and ignored entries. A malformed line voids the whole read.
#[test]
fn salvage_status_class_reports_only_untracked_and_ignored_entries() {
    assert_eq!(super::salvage_status_class(b""), Some((false, false)));
    assert_eq!(
        super::salvage_status_class(b" M a.txt\nA  b.txt\nR  c.txt -> d.txt\n"),
        Some((false, false)),
        "tracked entries, staged or not, need no class"
    );
    assert_eq!(
        super::salvage_status_class(b" M a.txt\n?? b.txt\n"),
        Some((true, false))
    );
    assert_eq!(
        super::salvage_status_class(b"!! target/\n"),
        Some((false, true))
    );
    assert_eq!(
        super::salvage_status_class(b"M a.txt\n"),
        None,
        "a one-column entry is not a line git emits"
    );
}

/// The net patch must carry original bytes: a configured textconv driver or
/// external diff would rewrite or empty file contents in the capture, and the
/// worktree is removed once the patch is durable. Both filters stay off.
#[test]
fn salvage_net_diff_disables_external_content_filters() {
    assert_eq!(
        super::salvage_argv_net_diff("/tree", SALVAGE_BASE),
        vec![
            "git".to_owned(),
            "-C".to_owned(),
            "/tree".to_owned(),
            "diff".to_owned(),
            "--binary".to_owned(),
            "--no-ext-diff".to_owned(),
            "--no-textconv".to_owned(),
            SALVAGE_BASE.to_owned(),
        ],
        "the salvage capture keeps raw bytes past textconv and ext-diff"
    );
}

/// Only a full object name reaches a git argv: 40 or 64 lowercase hex digits.
#[test]
fn only_a_full_object_name_counts_as_a_recorded_base() {
    assert!(super::valid_object_name(SALVAGE_BASE));
    assert!(super::valid_object_name(&"a".repeat(64)));
    let refused = [
        String::new(),
        "HEAD".to_owned(),
        "--output=x".to_owned(),
        SALVAGE_BASE.to_uppercase(),
        SALVAGE_BASE[..39].to_owned(),
        format!("{SALVAGE_BASE}0"),
        format!("{SALVAGE_BASE}\n"),
    ];
    for text in &refused {
        assert!(!super::valid_object_name(text), "{text:?}");
    }
}

/// Opens a passthrough owner over a fresh repo top and data root. Real git
/// answers the salvage calls; the jobs table stays scripted.
#[cfg(unix)]
async fn salvage_real_open(
    top: std::path::PathBuf,
    data: std::path::PathBuf,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let mut config = parse_config(None)?;
    config.goal.enabled = false;
    config.data_root = Some(data);
    let workspace = dal_core::Workspace::new(top).expect("absolute workspace");
    passthrough_fixture(config, workspace, None).await
}

/// Builds a seed repo at `top` plus a battery-layout worktree for `task`
/// with its creation receipt, which records the seed commit as the base the
/// tree started from. Runs blocking git; callers wrap in `spawn_blocking`.
#[cfg(unix)]
fn salvage_real_tree(
    top: &std::path::Path,
    data: &std::path::Path,
    session: SessionId,
    task: JobId,
) -> std::path::PathBuf {
    let base = seed_new_repo(top);
    let dir = data
        .join("worktrees")
        .join(session.to_string())
        .join(task.to_string());
    std::fs::create_dir_all(dir.parent().expect("session dir")).expect("parents");
    run_git(
        top,
        &[
            "worktree",
            "add",
            "--detach",
            dir.to_str().expect("utf-8 worktree dir"),
            "HEAD",
        ],
    );
    let receipt = data
        .join("isolation")
        .join(session.to_string())
        .join(task.to_string());
    std::fs::create_dir_all(&receipt).expect("receipt");
    std::fs::write(receipt.join("base.txt"), format!("{base}\n")).expect("receipt base");
    dir
}

#[cfg(unix)]
async fn worktree_line_count(top: std::path::PathBuf) -> usize {
    let out = tokio::task::spawn_blocking(move || run_git(&top, &["worktree", "list"]))
        .await
        .expect("worktree list joins");
    String::from_utf8_lossy(&out.stdout).lines().count()
}

/// The workspace the real-git owner opened over: the fresh repo top.
#[cfg(unix)]
fn salvage_workspace(top: &std::path::Path) -> dal_core::Workspace {
    dal_core::Workspace::new(top.to_path_buf()).expect("absolute workspace")
}

#[cfg(unix)]
fn fresh_roots(prefix: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let top = unique_temp_root(&format!("dalgona-salvage-{prefix}-top"));
    let data = unique_temp_root(&format!("dalgona-salvage-{prefix}"));
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&top).expect("top");
    std::fs::create_dir_all(&data).expect("data root");
    (top, data)
}

/// Untracked-only work stays in place: untracked bytes never enter the net
/// patch (no staging step runs), so the tree is kept with its contents and
/// its registration intact.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_keeps_untracked_only_work_in_place() -> TestResult {
    let (top, data) = fresh_roots("untracked");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir = tokio::task::spawn_blocking(move || {
        let dir = salvage_real_tree(&top_in, &data_in, session, task);
        std::fs::write(dir.join("notes.txt"), b"untracked work\n").expect("notes");
        dir
    })
    .await
    .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        assert!(
            dir.is_dir(),
            "the untracked tree stays in place with its bytes"
        );
        assert!(
            dir.join("notes.txt").is_file(),
            "the untracked file itself survives"
        );
        assert!(
            script.artifacts.is_empty(),
            "no patch merges untracked bytes"
        );
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        2,
        "its worktree registration stays intact"
    );
    let script = fixture.script();
    assert_eq!(script.notices.len(), 1);
    assert!(
        script.notices[0].text.contains(task.to_string().as_str())
            && script.notices[0].text.contains("untracked"),
        "the notice names the kept tree and why: {}",
        script.notices[0].text
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Staged plus unstaged tracked work must both land in the ONE net patch.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_captures_staged_and_unstaged_work() -> TestResult {
    let (top, data) = fresh_roots("mixed");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir = tokio::task::spawn_blocking(move || {
        let dir = salvage_real_tree(&top_in, &data_in, session, task);
        std::fs::write(dir.join("staged.txt"), b"staged change\n").expect("staged");
        let mut seed = std::fs::read_to_string(dir.join("seed.txt")).expect("seed");
        seed.push_str("unstaged change\n");
        std::fs::write(dir.join("seed.txt"), seed).expect("unstaged");
        run_git(&dir, &["add", "staged.txt"]);
        dir
    })
    .await
    .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        let patch = script
            .artifacts
            .iter()
            .find(|(job, file, _)| *job == task && *file == ArtifactFile::DeltaPatch)
            .map(|(_, _, bytes)| bytes.clone())
            .ok_or("the mixed delta was saved")?;
        for needle in ["staged change", "unstaged change"] {
            assert!(
                patch.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "the net patch carries {needle:?}"
            );
        }
        // The status travels through the artifact owner, not a bare file
        // write, and keeps the staging columns the merged patch cannot show.
        let (_, _, status_bytes) = script
            .artifacts
            .iter()
            .find(|(job, file, _)| *job == task && *file == ArtifactFile::StatusTxt)
            .ok_or("the status record was persisted through the artifact owner")?;
        let status = String::from_utf8_lossy(status_bytes);
        assert!(
            status.contains("seed.txt") && status.contains("staged.txt"),
            "the status record keeps both entries: {status}"
        );
        assert!(!dir.exists(), "the salvaged worktree is removed");
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        1,
        "only the main tree remains"
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// Work the task committed inside its tree is part of the net patch. The
/// patch is taken against the base the receipt records, not against the
/// tree's own `HEAD`, which the task's commit moved: a `HEAD`-relative patch
/// would drop the commit, and the removal would then destroy its only copy.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_keeps_work_the_task_committed_inside_its_tree() -> TestResult {
    let (top, data) = fresh_roots("committed");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir = tokio::task::spawn_blocking(move || {
        let dir = salvage_real_tree(&top_in, &data_in, session, task);
        std::fs::write(dir.join("committed.txt"), b"committed work\n").expect("committed");
        run_git(&dir, &["add", "committed.txt"]);
        run_git(
            &dir,
            &["-c", "commit.gpgsign=false", "commit", "-m", "task commit"],
        );
        let mut seed = std::fs::read_to_string(dir.join("seed.txt")).expect("seed");
        seed.push_str("later edit\n");
        std::fs::write(dir.join("seed.txt"), seed).expect("later edit");
        dir
    })
    .await
    .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        let patch = script
            .artifacts
            .iter()
            .find(|(job, file, _)| *job == task && *file == ArtifactFile::DeltaPatch)
            .map(|(_, _, bytes)| bytes.clone())
            .ok_or("the delta was saved")?;
        for needle in ["committed work", "later edit"] {
            assert!(
                patch.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "the net patch carries {needle:?}"
            );
        }
        assert!(!dir.exists(), "the salvaged worktree is removed");
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        1,
        "only the main tree remains"
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A tree whose every change is committed reads clean in `status`, yet it
/// holds work: the net patch against the base carries it, so the tree is
/// salvaged rather than reported as having no changes.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_keeps_work_that_is_only_committed_inside_its_tree() -> TestResult {
    let (top, data) = fresh_roots("committed-only");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir = tokio::task::spawn_blocking(move || {
        let dir = salvage_real_tree(&top_in, &data_in, session, task);
        std::fs::write(dir.join("only-committed.txt"), b"committed only\n").expect("committed");
        run_git(&dir, &["add", "only-committed.txt"]);
        run_git(
            &dir,
            &["-c", "commit.gpgsign=false", "commit", "-m", "task commit"],
        );
        dir
    })
    .await
    .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        let patch = script
            .artifacts
            .iter()
            .find(|(job, file, _)| *job == task && *file == ArtifactFile::DeltaPatch)
            .map(|(_, _, bytes)| bytes.clone())
            .ok_or("the committed delta was saved")?;
        let needle = "committed only";
        assert!(
            patch.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "the net patch carries the committed work"
        );
        assert!(!dir.exists(), "the salvaged worktree is removed");
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        1,
        "only the main tree remains"
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A tree that still equals its base holds no work to capture: it is left in
/// place with its registration, nothing is saved, and the notice says why.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_leaves_a_tree_that_still_equals_its_base() -> TestResult {
    let (top, data) = fresh_roots("clean");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir =
        tokio::task::spawn_blocking(move || salvage_real_tree(&top_in, &data_in, session, task))
            .await
            .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        assert!(dir.is_dir(), "the unchanged tree stays in place");
        assert!(script.artifacts.is_empty(), "no patch is saved");
        assert_eq!(script.notices.len(), 1);
        assert!(
            script.notices[0].text.contains(task.to_string().as_str())
                && script.notices[0].text.contains("no captured changes"),
            "the notice names the kept tree and why: {}",
            script.notices[0].text
        );
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        2,
        "its worktree registration stays intact"
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A foreign manually-placed directory parsing as a `JobId` is never touched:
/// without the creation receipt, and without worktree-list registration,
/// both stay in place with no removal.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_leaves_foreign_directories_in_place() -> TestResult {
    let (top, data) = fresh_roots("foreign");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, bare, receipted) = (fixture.session, JobId::new_v7(), JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    tokio::task::spawn_blocking(move || {
        seed_new_repo(&top_in);
        for task in [bare, receipted] {
            std::fs::create_dir_all(
                data_in
                    .join("worktrees")
                    .join(session.to_string())
                    .join(task.to_string()),
            )
            .expect("foreign dir");
        }
        // Only one carries a forged receipt; neither is a registered worktree.
        std::fs::create_dir_all(
            data_in
                .join("isolation")
                .join(session.to_string())
                .join(receipted.to_string()),
        )
        .expect("forged receipt");
    })
    .await
    .expect("setup joins");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    {
        let script = fixture.script();
        for task in [bare, receipted] {
            let dir = data
                .join("worktrees")
                .join(session.to_string())
                .join(task.to_string());
            assert!(dir.is_dir(), "a foreign dir stays in place: {dir:?}");
        }
        assert!(
            script.artifacts.is_empty(),
            "foreign dirs persist no patch and no status record"
        );
        assert!(
            !script
                .notices
                .iter()
                .any(|notice| notice.text.contains("kept at")),
            "nothing foreign is salvaged"
        );
        assert_eq!(script.notices.len(), 1, "one summary notice names both");
        assert!(
            script.notices[0].text.contains("left in place"),
            "the notice says why: {}",
            script.notices[0].text
        );
    }
    assert_eq!(
        worktree_line_count(top.clone()).await,
        1,
        "no foreign registration changed"
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// A sidecar persist failure keeps the tree and its registration, and skips
/// the prune: everything stays recoverable.
#[cfg(unix)]
#[tokio::test]
async fn open_salvage_failure_keeps_tree_and_skips_prune() -> TestResult {
    let (top, data) = fresh_roots("persist");
    let fixture = salvage_real_open(top.clone(), data.clone()).await?;
    let (session, task) = (fixture.session, JobId::new_v7());
    let (top_in, data_in) = (top.clone(), data.clone());
    let dir = tokio::task::spawn_blocking(move || {
        let dir = salvage_real_tree(&top_in, &data_in, session, task);
        std::fs::write(dir.join("work.txt"), b"base\n").expect("work");
        run_git(&dir, &["add", "work.txt"]);
        run_git(
            &dir,
            &["-c", "commit.gpgsign=false", "commit", "-m", "base"],
        );
        let mut work = std::fs::read_to_string(dir.join("work.txt")).expect("work");
        work.push_str("valued work\n");
        std::fs::write(dir.join("work.txt"), work).expect("valued");
        dir
    })
    .await
    .expect("setup joins");
    fixture.script().artifact_fail = Some("the artifact store is down");
    run_salvage_in(&fixture, &data, &salvage_workspace(&top)).await;
    let script = fixture.script();
    assert!(dir.is_dir(), "the unsaved tree stays in place");
    drop(script);
    assert_eq!(
        worktree_line_count(top.clone()).await,
        2,
        "its worktree registration stays intact"
    );
    let script = fixture.script();
    assert!(
        script.artifacts.is_empty(),
        "no patch and no status record was persisted"
    );
    assert_eq!(script.notices.len(), 1);
    assert!(
        script.notices[0].text.contains("could not be saved")
            && script.notices[0].text.contains("prune skipped"),
        "the notice names the failure and the skipped prune: {}",
        script.notices[0].text
    );
    let _ = std::fs::remove_dir_all(&top);
    let _ = std::fs::remove_dir_all(&data);
    Ok(())
}

/// The scale shape in miniature inside the battery crate: 500 children, 250
/// of them worktree writers merging one distinct real file each through the
/// real git executable, with the merge probes and the mailbox peak observed
/// from inside. Gated on `DALGONA_ORCH_STRESS` like the gates: the default
/// suite keeps the scripted boundary proofs above.
#[expect(
    clippy::disallowed_methods,
    reason = "the scale test honors the same stress opt-in flag as the gates"
)]
#[tokio::test]
async fn stress_five_hundred_children_merge_two_hundred_fifty_files() -> TestResult {
    if std::env::var_os("DALGONA_ORCH_STRESS").is_none() {
        return Ok(());
    }
    let (root, data, fixture, stress_files) = stress_open_fixture().await?;
    fixture.tool(&stress_run_request()).await?;
    let report = stress_wait_report(&fixture).await?;
    stress_verify_merges(&report, &fixture, &stress_files, &root).await?;
    stress_verify_checkout(&root).await?;
    let _ = tokio::fs::remove_dir_all(&root).await;
    let _ = tokio::fs::remove_dir_all(&data).await;
    Ok(())
}

/// The orchestration `git` helper against the real spawn admission. Every
/// other test in this file answers `run` from a script, so none of them can
/// see the stdout prefix cap the spawn door enforces; these drive a real
/// `dal_agent::Host` whose `run` service reaches `check_spawn_request`. The
/// cap is read from the door's own refusal, never copied, so the witnesses
/// fail the moment `git` asks for more than the door accepts.
#[cfg(unix)]
mod real_git_admission {
    use std::collections::BTreeMap;
    use std::error::Error;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dal_agent::error::ServiceError;
    use dal_agent::ext::{
        ArgError, BoxFuture, Caller, ExportSpec, ExtensionBuilder, RawValue, Services, Tool,
        ToolCall, ToolCx, ToolOutcome, ToolOutput,
    };
    use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
    use dal_core::ext::{ExportId, ExportKind, OpSet, Visibility};
    use dal_core::{
        Answer, ClientId, Command, Config, ConfigProduct, ExitStatusKind, Expect, ModelInfo, Name,
        Origin, Part, Question, RawJson, Reply, RunOutput, RunRequest, ServiceSet, ToolClass,
        ToolSpec, UpdateKind, Workspace,
    };

    use super::TestResult;

    /// Long enough for a cold host start and one real git run.
    const WAIT: Duration = Duration::from_secs(60);

    /// The stdout prefix limit orchestration git calls carried when every one
    /// of them was refused at spawn.
    const OLD_LIMIT: u32 = 1 << 20;

    /// The scripted model step that calls the probe tool once.
    const STEP_CALL: &str = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"git-call","name":"fixture__run_git"},{"type":"tool_calls_done","calls":[{"id":"git-call","name":"fixture__run_git","args":{"kind":"parsed","value":{}}}]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}"#;

    /// The scripted model step that ends the turn.
    const STEP_END: &str = r#"{"kind":"events","events":[{"type":"text_delta","text":"done"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#;

    /// One request the probe tool sends through the real `run` service.
    enum Op {
        /// An orchestration git call through the real `git` helper.
        Git(Vec<String>),
        /// A bare `git --version` run asking for this stdout prefix limit.
        Limit(u32),
    }

    /// What the probe saw, in request order: the output, or the refusal text.
    type Seen = Arc<Mutex<Vec<Result<RunOutput, String>>>>;

    /// A model-visible tool that sends its requests through the real `run`
    /// service of the session and records every answer.
    struct GitProbe {
        name: Name,
        spec: Arc<ToolSpec>,
        cwd: PathBuf,
        ops: Vec<Op>,
        seen: Seen,
    }

    impl GitProbe {
        /// Sends one request through the real `run` service.
        async fn send(
            &self,
            services: &dyn Services,
            caller: &Caller,
            op: &Op,
        ) -> Result<RunOutput, ServiceError> {
            let limit = match op {
                Op::Git(argv) => {
                    return super::super::git(services, caller, &self.cwd, argv.clone()).await;
                }
                Op::Limit(limit) => *limit,
            };
            let request = RunRequest {
                argv: vec![OsString::from("git"), OsString::from("--version")],
                cwd: Some(self.cwd.clone()),
                stdin: None,
                timeout: None,
                env: Vec::new(),
                stdout_prefix_limit: limit,
            };
            services.run(caller, request).await
        }
    }

    impl Tool for GitProbe {
        fn name(&self) -> &Name {
            &self.name
        }

        fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
            Arc::clone(&self.spec)
        }

        fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
            Ok(ToolClass::Other)
        }

        fn run<'a>(&'a self, _call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                let services = cx.services();
                for op in &self.ops {
                    let result = self.send(services.as_ref(), cx.caller(), op).await;
                    super::locked(&self.seen).push(result.map_err(|error| error.to_string()));
                }
                ToolOutcome::Ok(Box::new(ToolOutput::from_text("probed")))
            })
        }
    }

    /// How the session approves a run: `approval = "all"` runs without a
    /// question, `ask` opens one a front end answers.
    #[derive(Clone, Copy)]
    enum Approval {
        All,
        Ask,
    }

    /// What one driven session observed.
    struct Driven {
        /// One entry per probe request, in order.
        results: Vec<Result<RunOutput, String>>,
        /// `(title, body)` of every run approval the session opened.
        previews: Vec<(String, String)>,
    }

    /// Waits for the turn to end, answering every run approval with approve
    /// and recording its preview.
    async fn await_turn_end(
        agent: &Agent,
        subscription: &mut Subscription,
        previews: &mut Vec<(String, String)>,
    ) -> TestResult {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    if let Question::Approval { tool, preview, .. } = &request.question
                        && &**tool == "run"
                    {
                        previews.push((preview.title.to_string(), preview.body.to_string()));
                        agent.answer(request.id, Answer::Approve).await?;
                    }
                }
                UpdateKind::TurnEnded { .. } => return Ok(()),
                _ => {}
            }
        }
        Err("the session closed before the turn ended".into())
    }

    /// Runs the probe requests in a real host session under `approval = "all"`:
    /// nothing is asked and the requests run as the real door admits them.
    async fn drive_unasked(
        base: &Path,
        workspace: &Path,
        ops: Vec<Op>,
    ) -> Result<Driven, Box<dyn Error>> {
        drive(base, workspace, Approval::All, ops).await
    }

    /// Runs the probe requests in a real host session whose front end answers
    /// every run approval, recording the previews it was shown.
    async fn drive_asked(
        base: &Path,
        workspace: &Path,
        ops: Vec<Op>,
    ) -> Result<Driven, Box<dyn Error>> {
        drive(base, workspace, Approval::Ask, ops).await
    }

    /// Opens a real host session on `workspace`, lets a scripted model call
    /// the probe tool once, and returns what the probe saw. `base` holds the
    /// session's data, home, and cache.
    async fn drive(
        base: &Path,
        workspace: &Path,
        approval: Approval,
        ops: Vec<Op>,
    ) -> Result<Driven, Box<dyn Error>> {
        let seen = Seen::default();
        let probe: Arc<dyn Tool> = Arc::new(GitProbe {
            name: Name::parse("fixture__run_git")?,
            spec: Arc::new(ToolSpec {
                name: Name::parse("fixture__run_git")?,
                description: "git probe tool".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#)?,
                grammar: None,
            }),
            cwd: workspace.to_owned(),
            ops,
            seen: Arc::clone(&seen),
        });
        let previews = drive_tool(
            base,
            workspace,
            approval,
            probe,
            ServiceSet::from_names(["run"])?,
        )
        .await?;
        let results = std::mem::take(&mut *super::locked(&seen));
        Ok(Driven { results, previews })
    }

    /// Opens a real host session on `workspace`, lets a scripted model call
    /// `tool` once, and returns every run approval preview. `base` holds the
    /// session's data, home, and cache.
    async fn drive_tool(
        base: &Path,
        workspace: &Path,
        approval: Approval,
        tool: Arc<dyn Tool>,
        services: ServiceSet,
    ) -> Result<Vec<(String, String)>, Box<dyn Error>> {
        let data = base.join("data");
        tokio::fs::create_dir_all(&data).await?;
        let fixture = data.join("script.jsonl");
        tokio::fs::write(&fixture, format!("{STEP_CALL}\n{STEP_END}\n")).await?;
        let approval_line = match approval {
            Approval::All => "approval = \"all\"\n",
            Approval::Ask => "",
        };
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n{approval_line}\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        );
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))?;
        let extension = ExtensionBuilder::new("fixture", "0.1.0", services)?
            .with_origin(Origin::Builtin, None)
            .script_tool(
                tool,
                Visibility::Model,
                ExportSpec {
                    id: ExportId {
                        plugin: Name::parse("fixture")?,
                        kind: ExportKind::Tool,
                        local: Name::parse("run_git")?,
                    },
                    uses: OpSet::EMPTY,
                    input: RawJson::parse(r#"{"type":"object"}"#)?,
                    description: "git probe export".into(),
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
                (OsString::from("HOME"), base.join("home").into_os_string()),
                (
                    OsString::from("XDG_CACHE_HOME"),
                    base.join("cache").into_os_string(),
                ),
            ]),
            cwd: workspace.to_owned(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await?;
        let agent = host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(workspace.to_owned())?,
                    name: None,
                },
                ClientId::new("git-cap"),
            )
            .await?;
        let mut subscription = match approval {
            Approval::Ask => agent.subscribe(None)?,
            Approval::All => agent.subscribe_listen(None)?,
        };
        let reply = agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "probe git".into(),
                }],
            })
            .await?;
        if !matches!(reply, Reply::Accepted { .. }) {
            return Err(format!("prompt was not accepted: {reply:?}").into());
        }
        let mut previews = Vec::new();
        tokio::time::timeout(
            WAIT,
            await_turn_end(&agent, &mut subscription, &mut previews),
        )
        .await
        .map_err(|_| "the probe turn never ended")??;
        host.close(agent.session()).await?;
        Ok(previews)
    }

    /// A model-visible tool that runs the orchestration worktree-gate
    /// sequence through the real services: a scoped-grant `git worktree
    /// add` into a worktree root that does not pre-exist, a child start
    /// inside the created tree, and a child start in another session's
    /// tree. Records every answer in order.
    struct WorktreeGateProbe {
        name: Name,
        spec: Arc<ToolSpec>,
        top: PathBuf,
        data: PathBuf,
        base: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl WorktreeGateProbe {
        /// Starts one child in `workspace`, recording the reply verbatim.
        async fn start_child(&self, services: &dyn Services, caller: &Caller, workspace: PathBuf) {
            let seen = match Workspace::new(workspace) {
                Ok(workspace) => {
                    let start = dal_core::AgentStart {
                        call: dal_core::CallId::new("worktree-gate-child"),
                        name: Some("gate-child".into()),
                        prompt: "work".into(),
                        model: None,
                        role: None,
                        system: None,
                        tools: None,
                        workspace: Some(workspace),
                    };
                    match services
                        .agents(caller, dal_core::AgentsOp::Start(start))
                        .await
                    {
                        Ok(reply) => format!("start: {reply:?}"),
                        Err(error) => format!("start: error {error}"),
                    }
                }
                Err(error) => format!("start: workspace {error}"),
            };
            super::locked(&self.seen).push(seen);
        }
    }

    impl Tool for WorktreeGateProbe {
        fn name(&self) -> &Name {
            &self.name
        }

        fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
            Arc::clone(&self.spec)
        }

        fn classify(&self, _args: &RawValue, ws: &Workspace) -> Result<ToolClass, ArgError> {
            // The orchestration `agents` grant shape: the workspace plus the
            // worktree and isolation trees the run itself creates.
            Ok(ToolClass::Exec {
                read_only: false,
                grant: Some(dal_core::GrantSpec {
                    argv_prefix: "git".into(),
                    roots: vec![
                        ws.as_path().to_path_buf(),
                        self.data.join("worktrees"),
                        self.data.join("isolation"),
                    ],
                }),
            })
        }

        fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                let services = cx.services();
                let caller = cx.caller().clone();
                let session = cx.session();
                // The approval lends this call the scoped git grant the same
                // way the orchestration `agents` approval does before the
                // run drives git; without it the run calls ride the
                // workspace-only default and the worktree add denies.
                if let Err(error) = cx
                    .authorize(dal_core::Preview {
                        title: "run worktree gates".into(),
                        body: "Runs git in the workspace and in its own worktrees.".into(),
                        digest: None,
                    })
                    .await
                {
                    return ToolOutcome::Err(dal_agent::error::ToolError::Denied(error));
                }
                let dir = self
                    .data
                    .join("worktrees")
                    .join(session.to_string())
                    .join("task");
                let added = super::super::git(
                    services.as_ref(),
                    &caller,
                    &self.top,
                    super::super::super::worktree::argv_worktree_add(
                        &self.top.display().to_string(),
                        &dir.display().to_string(),
                        &self.base,
                    ),
                )
                .await;
                super::locked(&self.seen).push(match &added {
                    Ok(out) => format!("add: exited={}", out.status == ExitStatusKind::Exited(0)),
                    Err(error) => format!("add: refused {error}"),
                });
                self.start_child(services.as_ref(), &caller, dir).await;
                let foreign = self
                    .data
                    .join("worktrees")
                    .join("01900000-0000-7000-8000-000000000000")
                    .join("task");
                assert_ne!(session.to_string(), "01900000-0000-7000-8000-000000000000");
                if let Err(error) = tokio::fs::create_dir_all(&foreign).await {
                    super::locked(&self.seen).push(format!("foreign: error {error}"));
                } else {
                    self.start_child(services.as_ref(), &caller, foreign).await;
                }
                ToolOutcome::Ok(Box::new(ToolOutput::from_text("probed")))
            })
        }
    }

    /// Worktree-isolated children pass both admission gates on the real
    /// host: the `git worktree add` the run's own scoped grant covers is
    /// admitted even though the worktree root does not pre-exist (R4a),
    /// and the child started inside the created tree is admitted as this
    /// session's worktree child (R4b). A child aimed at another session's
    /// tree still refuses with `WorkspaceOutsideRoot`.
    #[tokio::test]
    async fn worktree_isolated_children_pass_both_gates_on_the_real_host() -> TestResult {
        let root = super::unique_temp_root("dalgona-worktree-gates");
        let base = root.join("main");
        let workspace = blob_checkout(&base, &[("note.txt", 8)]).await;
        let top = workspace.display().to_string();
        let head = tokio::task::spawn_blocking(move || {
            let out = super::run_git(&workspace, &["rev-parse", "HEAD"]);
            String::from_utf8(out.stdout)
                .expect("head utf-8")
                .trim()
                .to_owned()
        })
        .await
        .map_err(|error| format!("rev-parse joins: {error}"))?;
        let data = base.join("data");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let probe: Arc<dyn Tool> = Arc::new(WorktreeGateProbe {
            name: Name::parse("fixture__run_git")?,
            spec: Arc::new(ToolSpec {
                name: Name::parse("fixture__run_git")?,
                description: "worktree gate probe tool".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#)?,
                grammar: None,
            }),
            top: PathBuf::from(top),
            data: data.clone(),
            base: head,
            seen: Arc::clone(&seen),
        });
        drive_tool(
            &base,
            &base.join("w"),
            Approval::All,
            probe,
            ServiceSet::from_names(["run", "agents"])?,
        )
        .await?;
        let seen = std::mem::take(&mut *super::locked(&seen));
        let [added, started, foreign] = seen.as_slice() else {
            return Err(format!("the probe answered three gates, saw {seen:?}").into());
        };
        assert_eq!(
            added, "add: exited=true",
            "the worktree add passes its gate"
        );
        assert!(
            started.starts_with("start: Started"),
            "the child in this session's tree starts: {started}"
        );
        assert!(
            foreign.contains("WorkspaceOutsideRoot"),
            "another session's tree still refuses: {foreign}"
        );
        let _ = tokio::fs::remove_dir_all(&root).await;
        Ok(())
    }

    /// Reads the cap the real spawn door enforces from its own refusal of the
    /// old orchestration limit.
    fn enforced_cap(refusal: &Result<RunOutput, String>) -> Result<usize, Box<dyn Error>> {
        let message = refusal
            .as_ref()
            .err()
            .ok_or("the real spawn door admitted the old 1 MiB stdout prefix limit")?;
        let digits: String = message
            .split("exceeds ")
            .nth(1)
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().map_err(|_| -> Box<dyn Error> {
            format!("the refusal of the old limit names no cap: {message}").into()
        })
    }

    /// Opens a throwaway session to read the cap the real spawn door enforces.
    async fn probe_cap(root: &Path) -> Result<usize, Box<dyn Error>> {
        let base = root.join("probe");
        let workspace = base.join("w");
        tokio::fs::create_dir_all(&workspace).await?;
        let driven = drive_unasked(&base, &workspace, vec![Op::Limit(OLD_LIMIT)]).await?;
        let [refusal] = driven.results.as_slice() else {
            return Err("the cap probe made one request and saw another count".into());
        };
        enforced_cap(refusal)
    }

    /// The deterministic content of a `size`-byte blob.
    fn blob_bytes(size: usize) -> Vec<u8> {
        (0..size)
            .map(|index| b"abcdefghijklmnopqrstuvwxyz"[index % 26])
            .collect()
    }

    /// Makes `base/w` a git checkout committing one blob per `(name, size)`
    /// pair, and returns the workspace path. The real git runs on the
    /// blocking pool.
    async fn blob_checkout(base: &Path, blobs: &[(&str, usize)]) -> PathBuf {
        let workspace = base.join("w");
        let blobs: Vec<(String, usize)> = blobs
            .iter()
            .map(|(name, size)| ((*name).to_owned(), *size))
            .collect();
        let tree = workspace.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&tree).expect("workspace");
            super::seed_new_repo(&tree);
            for (name, size) in &blobs {
                std::fs::write(tree.join(name), blob_bytes(*size)).expect("blob");
            }
            super::run_git(&tree, &["add", "-A"]);
            super::run_git(
                &tree,
                &["-c", "commit.gpgsign=false", "commit", "-m", "blobs"],
            );
        })
        .await
        .expect("checkout setup joins");
        workspace
    }

    /// The git argv that prints one committed blob byte for byte.
    fn cat_blob(name: &str) -> Vec<String> {
        ["git", "cat-file", "blob", &format!("HEAD:{name}")]
            .map(str::to_owned)
            .to_vec()
    }

    /// Git output past the cap but under the old limit is admitted and kept
    /// to the cap. The old 1 MiB limit is itself refused by the same door, so
    /// this fails against it: the git call comes back refused, not run.
    #[tokio::test]
    async fn orchestration_git_output_past_the_cap_and_under_the_old_limit_is_admitted()
    -> TestResult {
        let root = super::unique_temp_root("dalgona-git-cap-between");
        let old = usize::try_from(OLD_LIMIT)?;
        let size = old / 2;
        let base = root.join("main");
        let workspace = blob_checkout(&base, &[("between.bin", size)]).await;
        let driven = drive_unasked(
            &base,
            &workspace,
            vec![Op::Limit(OLD_LIMIT), Op::Git(cat_blob("between.bin"))],
        )
        .await?;
        let [refusal, listed] = driven.results.as_slice() else {
            return Err("the probe made two requests and saw another count".into());
        };
        let cap = enforced_cap(refusal)?;
        assert!(
            cap < size && size < old,
            "the blob sits between the cap {cap} and the old limit {old}: {size}"
        );
        let out = listed.as_ref().map_err(|error| {
            format!("the real spawn door refused the orchestration git call: {error}")
        })?;
        assert_eq!(out.status, ExitStatusKind::Exited(0));
        assert_eq!(
            out.stdout_prefix.len(),
            cap,
            "the git call keeps exactly the cap the door enforces"
        );
        assert!(
            out.stdout_prefix_overflowed,
            "the bytes past the cap are flagged, not refused"
        );
        assert!(
            out.stdout_prefix == blob_bytes(size)[..cap],
            "the kept prefix is the head of the blob"
        );
        let _ = tokio::fs::remove_dir_all(&root).await;
        Ok(())
    }

    /// At the cap boundary the door truncates and flags: output of one byte
    /// under or exactly at the cap is whole and clean, and one byte over is
    /// cut to the cap and flagged.
    #[tokio::test]
    async fn orchestration_git_output_at_the_cap_boundary_is_whole_and_only_overflow_is_flagged()
    -> TestResult {
        let root = super::unique_temp_root("dalgona-git-cap-boundary");
        let cap = probe_cap(&root).await?;
        assert!(cap > 1, "the door names a usable cap: {cap}");
        let blobs = [
            ("below.bin", cap - 1),
            ("exact.bin", cap),
            ("above.bin", cap + 1),
        ];
        let base = root.join("main");
        let workspace = blob_checkout(&base, &blobs).await;
        let ops = blobs
            .iter()
            .map(|(name, _)| Op::Git(cat_blob(name)))
            .collect();
        let driven = drive_unasked(&base, &workspace, ops).await?;
        assert_eq!(driven.results.len(), blobs.len());
        for ((name, size), result) in blobs.iter().zip(&driven.results) {
            let out = result.as_ref().map_err(|error| {
                format!("{name}: the real spawn door refused the orchestration git call: {error}")
            })?;
            let kept = (*size).min(cap);
            assert_eq!(out.status, ExitStatusKind::Exited(0), "{name}");
            assert_eq!(out.stdout_prefix.len(), kept, "{name}: bytes kept");
            assert_eq!(
                out.stdout_prefix_overflowed,
                *size > cap,
                "{name}: only output past the cap is flagged"
            );
            assert!(
                out.stdout_prefix == blob_bytes(*size)[..kept],
                "{name}: the kept prefix is the head of the blob"
            );
        }
        let _ = tokio::fs::remove_dir_all(&root).await;
        Ok(())
    }

    /// The approval a front end answers for an orchestration git call shows
    /// the exact command the host then runs: the cwd and argv render
    /// losslessly on two lines (no control byte can hide or fake a line), and
    /// the call, once approved, prints the blob that exact command names.
    #[tokio::test]
    async fn the_approval_preview_shows_the_exact_git_call_the_real_host_runs() -> TestResult {
        let root = super::unique_temp_root("dalgona-git-cap-preview");
        let cap = probe_cap(&root).await?;
        let base = root.join("main");
        let workspace = blob_checkout(&base, &[("exact.bin", cap)]).await;
        let argv = cat_blob("exact.bin");
        let driven = drive_asked(&base, &workspace, vec![Op::Git(argv.clone())]).await?;
        let [(title, body)] = driven.previews.as_slice() else {
            return Err(format!(
                "one run approval opened for one git call, saw {:?}",
                driven.previews
            )
            .into());
        };
        assert!(
            argv.iter()
                .all(|arg| !arg.chars().any(char::is_control) && !arg.contains(' ')),
            "no argument can render ambiguously in the unescaped preview: {argv:?}"
        );
        assert_eq!(title, "run git");
        assert_eq!(
            body,
            &format!("cwd: {}\ncmd: {}", workspace.display(), argv.join(" "))
        );
        assert_eq!(body.lines().count(), 2, "{body:?}");
        let [result] = driven.results.as_slice() else {
            return Err("the probe made one request and saw another count".into());
        };
        let out = result.as_ref().map_err(|error| {
            format!("the approved orchestration git call was refused at spawn: {error}")
        })?;
        assert_eq!(out.status, ExitStatusKind::Exited(0));
        assert!(
            !out.stdout_prefix_overflowed && out.stdout_prefix == blob_bytes(cap),
            "the approved command printed exactly the blob it names"
        );
        let _ = tokio::fs::remove_dir_all(&root).await;
        Ok(())
    }
}

/// Git prefix admission: a git subprocess whose output exceeds the
/// documented prefix cap must be refused, not admitted unbounded or
/// silently cut. Uses the real cap constant and a real git spawn.
#[cfg(test)]
mod git_prefix_admission {
    use super::TestResult;
    use dal_core::{ExitStatusKind, RunOutput};

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the boundary test intentionally runs the real git executable"
    )]
    fn git_output_past_the_prefix_cap_is_refused_not_admitted() -> TestResult {
        let cap = usize::try_from(super::super::GIT_STDOUT_PREFIX_LIMIT)?;
        assert!(cap > 1024, "the cap is the real door limit: {cap}");
        let root = super::unique_temp_root("dalgona-git-prefix-refusal");
        let work = root.join("w");
        std::fs::create_dir_all(&work)?;
        super::seed_new_repo(&work);
        let size = cap + 1024;
        let blob: Vec<u8> = (0..size)
            .map(|index| b"abcdefghijklmnopqrstuvwxyz"[index % 26])
            .collect();
        std::fs::write(work.join("over.bin"), &blob)?;
        super::run_git(&work, &["add", "-A"]);
        super::run_git(
            &work,
            &["-c", "commit.gpgsign=false", "commit", "-m", "blob"],
        );
        // Real git spawn producing output past the prefix cap.
        let spawned = std::process::Command::new("git")
            .args(["cat-file", "blob", "HEAD:over.bin"])
            .current_dir(&work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()?;
        assert!(spawned.status.success(), "git cat-file runs");
        assert!(
            spawned.stdout.len() > cap,
            "the git output exceeds the cap {cap}: {}",
            spawned.stdout.len()
        );
        assert_eq!(
            spawned.stdout, blob,
            "the git output is the blob byte for byte"
        );
        // Shape the admission input exactly as the host door does: keep the
        // capped prefix and flag the overflow.
        let mut prefix = spawned.stdout;
        prefix.truncate(cap);
        let output = RunOutput {
            status: ExitStatusKind::Exited(0),
            stdout_tail: Vec::new(),
            stdout_prefix: prefix,
            stdout_prefix_overflowed: true,
            stderr_tail: Vec::new(),
            log: None,
        };
        assert!(
            !super::super::complete(&output),
            "output past the cap is not complete"
        );
        assert!(
            super::super::git_refusal(&Ok(output)).is_some(),
            "output past the cap must be refused, not admitted as success"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
