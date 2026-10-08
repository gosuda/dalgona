// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Deserialize;

use dal_agent::error::ServiceError;
use dal_agent::ext::{
    BoxFuture, Caller, Hook, HookCx, HookError, ObserveHook, Services, StatusCx, StatusPoll,
    StatusSnapshot,
};
use dal_core::ext::{
    InputEvent, InputVerdict, SessionEnd, SessionStart, Settled, ToolCallEvent, ToolCallVerdict,
    ToolResultEvent, TurnEnd,
};
use dal_core::{
    AgentReport, AgentState, AgentsOp, AgentsReply, CallId, JobId, JobStateView, JobsOp, JobsReply,
    Name, Notice, RawJson, SessionId, TurnOp,
};
use sonic_rs::JsonContainerTrait;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant as TokioInstant, timeout_at};

use super::agents_tool::{ReportCell, ReportOutcome, ReportStatus, submit};
use super::arbiter::Arbiter;
use super::goal::adapter::{self, GoalStore};
use super::goal::ops::{GoalScope, TodoSummary};
use super::monitor::state::{MonitorConfig, MonitorState};
use super::monitor::status::{InflightCounts, status_json, status_payload};
use super::stuck::{
    GuardState, GuardVerdict, SleepClassifier, clear_pending_attempts, on_tool_call, reset,
    rewrite_exec_args,
};
use super::{JobsView, OrchestrationConfig, StopKind};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct Runtime {
    owners: Arc<Mutex<HashMap<SessionId, Owner>>>,
    config: Arc<OrchestrationConfig>,
    sleep: Option<Arc<SleepClassifier>>,
}

struct Owner {
    sender: mpsc::Sender<Message>,
    snapshot: Arc<Mutex<StatusSnapshot>>,
    _run: tokio_util::task::AbortOnDropHandle<()>,
}

enum Message {
    Input(oneshot::Sender<Result<(), ServiceError>>),
    ToolCall(ToolCallEvent, oneshot::Sender<ToolCallVerdict>),
    Tool {
        call: CallId,
        name: Box<str>,
        args: RawJson,
        reply: oneshot::Sender<Result<String, ServiceError>>,
    },
    ToolResult(ToolResultEvent, oneshot::Sender<()>),
    TurnEnd(TurnEnd, oneshot::Sender<()>),
    Settled(Settled, oneshot::Sender<Result<(), ServiceError>>),
    Command {
        name: Box<str>,
        args: Box<str>,
        reply: oneshot::Sender<Result<String, ServiceError>>,
    },
    Close(oneshot::Sender<()>),
}

struct SessionState {
    session: SessionId,
    parent: Option<SessionId>,
    caller: Caller,
    services: Arc<dyn Services>,
    receiver: mpsc::Receiver<Message>,
    snapshot: Arc<Mutex<StatusSnapshot>>,
    config: Arc<OrchestrationConfig>,
    guard: GuardState,
    arbiter: Arbiter,
    sleep: Option<Arc<SleepClassifier>>,
    goal: Option<GoalStore>,
    report: ReportCell,
    monitors: MonitorState,
    open_asks: HashSet<CallId>,
    inflight_jobs: usize,
    last_stop: StopKind,
    turn_tool_called: bool,
    goal_timer: Option<(TokioInstant, String)>,
}

impl Runtime {
    pub(crate) fn new(config: OrchestrationConfig) -> Result<Self, super::RegistrationError> {
        let sleep = if config.sleep.enabled {
            Some(Arc::new(
                SleepClassifier::new().map_err(|_| super::RegistrationError::InvalidParameters)?,
            ))
        } else {
            None
        };
        Ok(Self {
            owners: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            sleep,
        })
    }

    pub(crate) fn config(&self) -> &OrchestrationConfig {
        &self.config
    }

    fn sender(&self, session: SessionId) -> Option<mpsc::Sender<Message>> {
        self.owners
            .lock()
            .ok()
            .and_then(|owners| owners.get(&session).map(|owner| owner.sender.clone()))
    }

    async fn hook_request<T>(
        &self,
        session: SessionId,
        deadline: TokioInstant,
        cancel: tokio_util::sync::CancellationToken,
        build: impl FnOnce(oneshot::Sender<T>) -> Message,
    ) -> Result<T, HookError>
    where
        T: Send + 'static,
    {
        let Some(sender) = self.sender(session) else {
            return Err(failed("orchestration owner is not running"));
        };
        let (reply, response) = oneshot::channel();
        let message = build(reply);
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HookError::Cancelled),
            sent = timeout_at(deadline, sender.send(message)) => match sent {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(failed("orchestration owner is closed")),
                Err(_) => return Err(failed("orchestration hook deadline exceeded")),
            }
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(HookError::Cancelled),
            received = timeout_at(deadline, response) => match received {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_)) => Err(failed("orchestration owner is closed")),
                Err(_) => Err(failed("orchestration hook deadline exceeded")),
            }
        }
    }

    pub(crate) async fn tool(
        &self,
        session: SessionId,
        call: CallId,
        name: &str,
        args: RawJson,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, ServiceError> {
        let Some(sender) = self.sender(session) else {
            return Err(ServiceError::failed(
                None,
                "orchestration owner is not running",
            ));
        };
        let (reply, response) = oneshot::channel();
        let message = Message::Tool {
            call,
            name: name.into(),
            args,
            reply,
        };
        let deadline = TokioInstant::now() + std::time::Duration::from_secs(600);
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ServiceError::Cancelled),
            sent = timeout_at(deadline, sender.send(message)) => match sent {
                Ok(Ok(())) => {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => Err(ServiceError::Cancelled),
                        received = timeout_at(deadline, response) => match received {
                            Ok(Ok(result)) => result,
                            Ok(Err(_)) => Err(ServiceError::failed(None, "orchestration owner is closed")),
                            Err(_) => Err(ServiceError::failed(None, "orchestration tool deadline exceeded")),
                        }
                    }
                }
                Ok(Err(_)) => Err(ServiceError::failed(None, "orchestration owner is closed")),
                Err(_) => Err(ServiceError::failed(None, "orchestration tool deadline exceeded")),
            }
        }
    }

    pub(crate) async fn command(
        &self,
        session: SessionId,
        name: &str,
        args: &str,
    ) -> Result<String, ServiceError> {
        let Some(sender) = self.sender(session) else {
            return Err(ServiceError::failed(
                None,
                "orchestration owner is not running",
            ));
        };
        let (reply, response) = oneshot::channel();
        let message = Message::Command {
            name: name.into(),
            args: args.into(),
            reply,
        };
        let deadline = TokioInstant::now() + std::time::Duration::from_secs(60);
        timeout_at(deadline, sender.send(message))
            .await
            .map_err(|_| ServiceError::failed(None, "orchestration command deadline exceeded"))?
            .map_err(|_| ServiceError::failed(None, "orchestration owner is closed"))?;
        timeout_at(deadline, response)
            .await
            .map_err(|_| ServiceError::failed(None, "orchestration command deadline exceeded"))?
            .map_err(|_| ServiceError::failed(None, "orchestration owner is closed"))?
    }

    async fn open(&self, start: SessionStart, cx: HookCx) -> Result<(), HookError> {
        let session = cx.session;
        let parent = cx.parent;
        let caller = cx.caller.clone();
        let services = Arc::clone(&cx.services);
        if parent.is_some() && self.config.agents.enabled {
            let report_tool =
                super::tools::report_tool(self).map_err(|error| failed(&error.to_string()))?;
            services
                .add_session_tools(&caller, vec![report_tool])
                .await
                .map_err(|error| failed(&error.to_string()))?;
        }
        let (sender, receiver) = mpsc::channel(256);
        let snapshot = Arc::new(Mutex::new(StatusSnapshot {
            quiet: true,
            text: None,
        }));
        let state = SessionState {
            session,
            parent,
            caller,
            services,
            receiver,
            snapshot: Arc::clone(&snapshot),
            config: Arc::clone(&self.config),
            guard: GuardState::default(),
            arbiter: Arbiter::new(),
            sleep: self.sleep.clone(),
            goal: None,
            report: ReportCell::default(),
            monitors: MonitorState::default(),
            open_asks: HashSet::new(),
            inflight_jobs: 0,
            last_stop: StopKind::Completed,
            turn_tool_called: false,
            goal_timer: None,
        };
        let _ = start;
        #[expect(
            clippy::disallowed_methods,
            reason = "the session registry owns and cancels this per-session task"
        )]
        let run = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(state.run()));
        if let Ok(mut owners) = self.owners.lock() {
            owners.insert(
                session,
                Owner {
                    sender,
                    snapshot,
                    _run: run,
                },
            );
        }
        let _ = start;
        Ok(())
    }

    async fn close(&self, session: SessionId, _end: SessionEnd) {
        if let Some(sender) = self.sender(session) {
            let (reply, response) = oneshot::channel();
            if sender.send(Message::Close(reply)).await.is_ok() {
                let _ = timeout_at(
                    TokioInstant::now() + std::time::Duration::from_secs(5),
                    response,
                )
                .await;
            }
        }
        if let Ok(mut owners) = self.owners.lock() {
            owners.remove(&session);
        }
    }

    fn snapshot(&self, session: SessionId) -> Option<StatusSnapshot> {
        let owners = self.owners.lock().ok()?;
        let owner = owners.get(&session)?;
        owner.snapshot.lock().ok().map(|snapshot| snapshot.clone())
    }
}

impl SessionState {
    async fn run(mut self) {
        if self.config.goal.enabled && self.parent.is_none() {
            self.load_goal().await;
        }
        self.publish_status();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                message = self.receiver.recv() => {
                    let Some(message) = message else {
                        return;
                    };
                    if self.handle_message(message).await {
                        return;
                    }
                }
                _ = ticker.tick() => {
                    self.tick().await;
                }
            }
        }
    }

    async fn handle_message(&mut self, message: Message) -> bool {
        match message {
            Message::Input(reply) => {
                let result = self.input().await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::ToolCall(event, reply) => {
                if event.tool.as_str() == "ask" {
                    self.open_asks.insert(event.call.clone());
                }
                self.turn_tool_called = true;
                let verdict = self.tool_call(event).await;
                self.publish_status();
                let _ = reply.send(verdict);
            }
            Message::Tool {
                call,
                name,
                args,
                reply,
            } => {
                let result = self.tool(call, &name, &args).await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::ToolResult(event, reply) => {
                self.open_asks.remove(&event.call);
                self.publish_status();
                let _ = reply.send(());
            }
            Message::TurnEnd(event, reply) => {
                if self.config.loop_guard.enabled {
                    clear_pending_attempts(&mut self.guard);
                }
                self.last_stop = stop_kind(&event.stop);
                self.turn_tool_called = false;
                self.publish_status();
                let _ = reply.send(());
            }
            Message::Settled(event, reply) => {
                let result = self.settled(event).await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::Command { name, args, reply } => {
                let result = self.command(&name, &args).await;
                self.publish_status();
                let _ = reply.send(result);
            }
            Message::Close(reply) => {
                self.close().await;
                let _ = reply.send(());
                return true;
            }
        }
        false
    }

    async fn tick(&mut self) {
        if let Some((deadline, prompt)) = self.goal_timer.as_ref()
            && TokioInstant::now() >= *deadline
        {
            let prompt = prompt.clone();
            self.goal_timer = None;
            self.arbiter.admit_goal(prompt, Instant::now());
        }
        let _ = self.deliver_ready().await;
        self.publish_status();
    }

    async fn deliver_ready(&mut self) -> Result<(), ServiceError> {
        if !self.open_asks.is_empty() {
            return Ok(());
        }
        let idle = match self.services.turn(&self.caller, TurnOp::IsIdle).await? {
            dal_core::TurnOpReply::Idle(idle) => idle,
            _ => false,
        };
        if !idle {
            return Ok(());
        }
        let jobs = self.completed_jobs().await?;
        let ready = self.arbiter.collect(jobs);
        if ready.is_empty() {
            return Ok(());
        }
        let (text, sources, job_ids) = self
            .arbiter
            .compose(&ready, super::arbiter::INJECTION_BUDGET);
        if text.is_empty() {
            self.requeue_ready(&ready);
            return Ok(());
        }
        let sources = sources.into_iter().map(Into::into).collect();
        match self
            .services
            .turn(
                &self.caller,
                TurnOp::Wake {
                    text: text.into_boxed_str(),
                    sources,
                    job_ids: job_ids.clone(),
                },
            )
            .await
        {
            Ok(dal_core::TurnOpReply::Woken) => self.arbiter.commit(&job_ids),
            Ok(_) => {
                self.arbiter.release(&job_ids);
                self.requeue_ready(&ready);
            }
            Err(ServiceError::Denied(dal_core::DenyReason::WakeLimit)) => {
                self.arbiter.stop();
                self.arbiter.release(&job_ids);
                self.requeue_ready(&ready);
            }
            Err(error) => {
                self.arbiter.release(&job_ids);
                self.requeue_ready(&ready);
                return Err(error);
            }
        }
        Ok(())
    }

    async fn completed_jobs(&mut self) -> Result<Vec<super::arbiter::JobReport>, ServiceError> {
        let jobs = self.jobs_list().await?;
        let mut reports = Vec::new();
        for job in jobs {
            if let JobStateView::Done(_) = job.state
                && !self.arbiter.is_committed(&job.id)
            {
                let text = match self
                    .services
                    .jobs(&self.caller, JobsOp::Text { id: job.id })
                    .await?
                {
                    JobsReply::Text { text, .. } => text.to_string(),
                    _ => String::new(),
                };
                reports.push(super::arbiter::JobReport {
                    id: job.id,
                    text,
                    from_run: false,
                });
            }
        }
        Ok(reports)
    }

    fn requeue_ready(&mut self, ready: &[super::arbiter::Ready]) {
        for item in ready {
            match item {
                super::arbiter::Ready::Recovery(text) => {
                    self.arbiter.admit_recovery(text.clone(), Instant::now());
                }
                super::arbiter::Ready::Monitor(batches) => {
                    for batch in batches {
                        self.arbiter.push_monitor(batch.clone(), Instant::now());
                    }
                }
                super::arbiter::Ready::Goal(text) => {
                    self.arbiter.admit_goal(text.clone(), Instant::now());
                }
                super::arbiter::Ready::Jobs(_) => {}
            }
        }
    }

    async fn load_goal(&mut self) {
        let session = self.session.to_string();
        self.goal = Some(adapter::load(self.services.as_ref(), &self.caller, &session).await);
        if self
            .goal
            .as_ref()
            .is_some_and(|store| adapter::persisted_mode(store) == Some("stopped"))
        {
            self.arbiter.stop();
        }
    }

    async fn input(&mut self) -> Result<(), ServiceError> {
        if self.config.loop_guard.enabled {
            reset(&mut self.guard);
        }
        self.arbiter.on_user_prompt();
        if let Some(store) = self.goal.as_mut() {
            let mode = self.arbiter.mode();
            let should_save = store.saved && store.sidecar.is_some();
            if let Some(sidecar) = store.sidecar.as_mut() {
                if let Some(goal) = sidecar.goal.as_mut() {
                    super::goal::policy::on_user_prompt(goal);
                }
                sidecar.controller = mode;
            }
            if should_save {
                let Some(sidecar) = store.sidecar.as_ref() else {
                    return Ok(());
                };
                adapter::save(self.services.as_ref(), &self.caller, sidecar).await?;
            }
        }
        Ok(())
    }

    async fn close(&mut self) {
        self.goal_timer = None;
        super::monitor::state::stop_all(&mut self.monitors);
        let sweep = cancel_descendants(self.services.as_ref(), &self.caller).await;
        if let Some(failures) = sweep.failure_text() {
            self.services.notify(
                &self.caller,
                Notice {
                    turn: None,
                    kind: "orchestration.cancel".into(),
                    text: format!(
                        "Some child sessions were not cancelled when the session ended: {failures}. They may still be running."
                    )
                    .into(),
                },
            );
        }
    }

    async fn command(&mut self, name: &str, args: &str) -> Result<String, ServiceError> {
        if self.parent.is_some() {
            return Ok(super::monitor::status::subagent_reply(name));
        }
        match name {
            "goal" => self.goal_command(args).await,
            "continuation" => self.continuation_command(args).await,
            "abort" => self.abort_command().await,
            _ => Err(ServiceError::failed(None, "unknown orchestration command")),
        }
    }

    async fn goal_command(&mut self, args: &str) -> Result<String, ServiceError> {
        let store = self.goal.as_mut().ok_or_else(|| {
            ServiceError::failed(None, "goal: the session store is not available.")
        })?;
        let session = self.session.to_string();
        let ctx = GoalScope {
            session: &session,
            saved: store.saved,
            depth: 0,
        };
        adapter::command(args, store, &ctx, self.services.as_ref(), &self.caller).await
    }

    async fn continuation_command(&mut self, args: &str) -> Result<String, ServiceError> {
        let reply = match args.trim() {
            "run" => {
                self.arbiter.on_continuation_run();
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            "pause" => {
                self.arbiter.pause("paused by the user");
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            "stop" => {
                self.arbiter.stop();
                super::goal::ops::continuation_line(&self.arbiter.mode())
            }
            _ => return Ok(super::goal::ops::continuation_unknown()),
        };
        self.persist_controller(&reply).await
    }

    async fn persist_controller(&mut self, reply: &str) -> Result<String, ServiceError> {
        let Some(store) = self.goal.as_mut() else {
            return Ok(reply.to_owned());
        };
        let mode = self.arbiter.mode();
        adapter::update_mode(store, mode);
        if !store.saved {
            return Ok(super::goal::ops::continuation_unsaved(
                reply,
                "this session has no sidecar store",
            ));
        }
        let Some(sidecar) = store.sidecar.as_ref() else {
            return Ok(super::goal::ops::continuation_unsaved(
                reply,
                "the goal sidecar is unavailable",
            ));
        };
        match adapter::save(self.services.as_ref(), &self.caller, sidecar).await {
            Ok(()) => Ok(reply.to_owned()),
            Err(error) => Ok(super::goal::ops::continuation_unsaved(
                reply,
                &error.to_string(),
            )),
        }
    }

    async fn abort_command(&mut self) -> Result<String, ServiceError> {
        let turn_was_running = match self.services.turn(&self.caller, TurnOp::IsIdle).await? {
            dal_core::TurnOpReply::Idle(idle) => !idle,
            _ => false,
        };
        if turn_was_running {
            let _ = self.services.turn(&self.caller, TurnOp::Cancel).await;
        }
        let jobs = self.jobs_list().await?;
        let mut jobs_cancelled = 0;
        for job in jobs
            .iter()
            .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
        {
            if self
                .services
                .jobs(&self.caller, JobsOp::Cancel { id: job.id })
                .await
                .is_ok()
            {
                jobs_cancelled += 1;
            }
        }
        let monitors_stopped = super::monitor::state::stop_all(&mut self.monitors);
        let sweep = cancel_descendants(self.services.as_ref(), &self.caller).await;
        self.goal_timer = None;
        self.arbiter.on_abort();
        let mut reply =
            super::monitor::status::abort_reply(turn_was_running, jobs_cancelled, monitors_stopped);
        if let Some(failures) = sweep.failure_text() {
            reply.push_str(" Some child sessions were not cancelled: ");
            reply.push_str(&failures);
            reply.push_str(". Run /abort again to retry.");
        }
        self.persist_controller(&reply).await
    }

    async fn tool(
        &mut self,
        call: CallId,
        name: &str,
        args: &RawJson,
    ) -> Result<String, ServiceError> {
        match name {
            "create_goal" | "update_goal" | "get_goal" => self.goal_tool(name, args.as_str()).await,
            "monitor" => self.monitor_tool(args).await,
            "report" => self.report_tool(args.as_str()),
            "agents" => self.agents_tool(call, args).await,
            _ => Err(ServiceError::failed(None, "unknown orchestration tool")),
        }
    }

    async fn goal_tool(&mut self, name: &str, args: &str) -> Result<String, ServiceError> {
        if self.parent.is_some() {
            return Err(ServiceError::failed(
                None,
                "goal: a subagent cannot hold a goal. Report to the agent that started you.",
            ));
        }
        let todos = self.todo_summary().await?;
        let inflight = self.inflight_counts().await?;
        let store = self.goal.as_mut().ok_or_else(|| {
            ServiceError::failed(None, "goal: the session store is not available.")
        })?;
        let session = self.session.to_string();
        let ctx = GoalScope {
            session: &session,
            saved: store.saved,
            depth: 0,
        };
        adapter::tool(
            name,
            args,
            store,
            &ctx,
            &todos,
            &inflight,
            self.services.as_ref(),
            &self.caller,
        )
        .await
    }

    async fn monitor_tool(&mut self, args: &RawJson) -> Result<String, ServiceError> {
        let request = super::monitor::state::parse_request(args)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let jobs = self.jobs_list().await?;
        let view = SessionJobsView { jobs: &jobs };
        let config = MonitorConfig {
            enabled: self.config.monitor.enabled,
            coalesce_ms: self.config.monitor.coalesce_ms,
            rate_limit_ms: self.config.monitor.rate_limit_ms,
            max_lines: self.config.monitor.max_lines,
            max_chars: self.config.monitor.max_chars,
            wake_budget: self.config.monitor.wake_budget,
        };
        let reply = super::monitor::state::watch(
            &mut self.monitors,
            &request,
            &view,
            dal_core::Timestamp::now(),
            &config,
        )
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        Ok(reply.text())
    }

    fn report_tool(&self, args: &str) -> Result<String, ServiceError> {
        if self.parent.is_none() {
            return Err(ServiceError::failed(
                None,
                super::agents_tool::NO_NESTED_RUNS,
            ));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ReportArgs {
            status: String,
            report: String,
        }
        let input: ReportArgs = sonic_rs::from_str(args)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let status = match input.status.as_str() {
            "done" => ReportStatus::Done,
            "blocked" => ReportStatus::Blocked,
            "failed" => ReportStatus::Failed,
            _ => return Err(ServiceError::failed(None, "report: status is invalid.")),
        };
        let (result, text) = submit(&self.report, status, &input.report);
        match result {
            ReportOutcome::Empty | ReportOutcome::Stored | ReportOutcome::Duplicate => {
                Ok(text.to_owned())
            }
        }
    }

    async fn jobs_list(&mut self) -> Result<Vec<dal_core::JobStatus>, ServiceError> {
        let jobs = match self.services.jobs(&self.caller, JobsOp::List).await? {
            JobsReply::Listed(jobs) => jobs,
            JobsReply::Unavailable { reason } => return Err(ServiceError::failed(None, reason)),
            _ => {
                return Err(ServiceError::failed(
                    None,
                    "jobs service returned an unexpected reply",
                ));
            }
        };
        self.inflight_jobs = jobs
            .iter()
            .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
            .count();
        Ok(jobs)
    }

    async fn todo_summary(&self) -> Result<TodoSummary, ServiceError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TodoRecord {
            list: Vec<TodoItem>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TodoItem {
            subject: String,
            state: String,
        }
        let records = self.services.records(&self.caller, "todo").await?;
        let Some(record) = records.last() else {
            return Ok(TodoSummary {
                open: 0,
                total: 0,
                first_titles: Vec::new(),
            });
        };
        let decoded: TodoRecord = sonic_rs::from_str(record.as_str())
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        let mut first_titles = Vec::with_capacity(3);
        let mut open = 0;
        for item in &decoded.list {
            if matches!(item.state.as_str(), "pending" | "in_progress") {
                open += 1;
                if first_titles.len() < 3 {
                    first_titles.push(item.subject.clone().into_boxed_str());
                }
            } else if !matches!(item.state.as_str(), "done" | "cancelled") {
                return Err(ServiceError::failed(
                    None,
                    "todo record has an unknown state",
                ));
            }
        }
        Ok(TodoSummary {
            open,
            total: decoded.list.len(),
            first_titles,
        })
    }

    fn live_parts(inflight: &InflightCounts) -> Vec<Box<str>> {
        let mut parts = Vec::new();
        if inflight.jobs > 0 {
            parts.push(format!("{} jobs", inflight.jobs).into_boxed_str());
        }
        if inflight.monitors > 0 {
            parts.push(format!("{} monitors", inflight.monitors).into_boxed_str());
        }
        if inflight.asks > 0 {
            parts.push("asks".into());
        }
        if inflight.goal_timer > 0 {
            parts.push("goal".into());
        }
        if inflight.loop_guard > 0 {
            parts.push("loop guard".into());
        }
        parts
    }

    async fn inflight_counts(&mut self) -> Result<InflightCounts, ServiceError> {
        let _ = self.jobs_list().await?;
        Ok(InflightCounts {
            jobs: self.inflight_jobs,
            monitors: self.monitors.live_count(),
            asks: self.open_asks.len(),
            goal_timer: u8::from(self.goal_timer.is_some()),
            loop_guard: u8::from(self.guard.episode.is_some()),
        })
    }

    async fn settled(&mut self, event: Settled) -> Result<(), ServiceError> {
        if self.parent.is_some() || self.goal.is_none() {
            return Ok(());
        }
        let todos = self.todo_summary().await?;
        let inflight = self.inflight_counts().await?;
        let idle = match self.services.turn(&self.caller, TurnOp::IsIdle).await? {
            dal_core::TurnOpReply::Idle(value) => value,
            _ => false,
        };
        let now = dal_core::Timestamp::now();
        let signature = {
            let Some(goal) = self
                .goal
                .as_ref()
                .and_then(|store| store.sidecar.as_ref())
                .and_then(|sidecar| sidecar.goal.as_ref())
            else {
                return Ok(());
            };
            if goal.status != super::GoalStatus::Active {
                return Ok(());
            }
            super::goal::policy::progress_signature(
                &goal.id,
                todos.open,
                todos.total,
                &event.reply_text,
            )
        };
        let decision = {
            let Some(goal) = self
                .goal
                .as_ref()
                .and_then(|store| store.sidecar.as_ref())
                .and_then(|sidecar| sidecar.goal.as_ref())
            else {
                return Ok(());
            };
            let input = super::goal::policy::VerdictInput {
                goal,
                path: super::goal::policy::GoalPath::AfterTurn,
                idle,
                pending_user_messages: false,
                continuation_pending: self.goal_timer.is_some(),
                last_turn_context_overflow: false,
                last_stop: self.last_stop,
                signature: &signature,
                open_todos: todos.open,
                total_todos: todos.total,
                inflight: &inflight,
            };
            super::goal::policy::verdict(&input)
        };
        if let Some(store) = self.goal.as_mut()
            && let Some(sidecar) = store.sidecar.as_mut()
            && let Some(goal) = sidecar.goal.as_mut()
        {
            match decision {
                super::goal::policy::Verdict::Continue { prompt, stall } => {
                    let live_parts = Self::live_parts(&inflight);
                    let number = goal.unattended.saturating_add(1);
                    let prompt_text = super::goal::prompt::build_prompt(
                        goal,
                        prompt,
                        number,
                        if stall { &live_parts } else { &[] },
                    );
                    super::goal::policy::record_goal_turn(
                        goal,
                        &event.reply_text,
                        self.turn_tool_called,
                        0,
                        0,
                        &signature,
                        prompt,
                    );
                    goal.updated_at = now;
                    self.goal_timer = Some((
                        TokioInstant::now()
                            + std::time::Duration::from_millis(
                                super::goal::policy::CONTINUATION_DELAY_MS,
                            ),
                        prompt_text,
                    ));
                }
                super::goal::policy::Verdict::Deny(reason) => {
                    if let Some(blocked) = reason.mechanical_reason() {
                        goal.status = super::GoalStatus::Blocked;
                        goal.blocked = Some(super::goal::sidecar::BlockedReason {
                            reason: blocked.into(),
                            at: now,
                            mechanical: true,
                        });
                        goal.updated_at = now;
                    }
                }
            }
        }
        if let Some(store) = self.goal.as_ref()
            && store.saved
            && let Some(sidecar) = store.sidecar.as_ref()
        {
            adapter::save(self.services.as_ref(), &self.caller, sidecar).await?;
        }
        Ok(())
    }

    async fn agents_tool(&mut self, call: CallId, args: &RawJson) -> Result<String, ServiceError> {
        if self.parent.is_some() {
            return Err(ServiceError::failed(
                None,
                super::agents_tool::NO_NESTED_RUNS,
            ));
        }
        let saved = self
            .config
            .workflows
            .as_ref()
            .and_then(|workflows| sonic_rs::to_string(workflows).ok())
            .and_then(|text| RawJson::parse(&text).ok());
        let action = super::agents_tool::decode_action(args, saved.as_ref())
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        match action {
            super::agents_tool::AgentAction::Wait { ids, timeout_s } => {
                let mut reports = Vec::new();
                for display in ids {
                    let id = SessionId::parse(&display).map_err(|_| {
                        ServiceError::failed(None, super::agents_tool::unknown_id(&display))
                    })?;
                    match self
                        .services
                        .agents(
                            &self.caller,
                            AgentsOp::Await {
                                id,
                                timeout: Some(std::time::Duration::from_secs(u64::from(timeout_s))),
                            },
                        )
                        .await?
                    {
                        AgentsReply::Await { report } => reports.push(report.text.to_string()),
                        _ => {
                            return Err(ServiceError::failed(
                                None,
                                super::agents_tool::unknown_id(&display),
                            ));
                        }
                    }
                }
                Ok(reports.join("\n\n"))
            }
            super::agents_tool::AgentAction::Cancel { ids } => {
                let ids = ids
                    .iter()
                    .map(|display| {
                        SessionId::parse(display).map_err(|_| {
                            ServiceError::failed(None, super::agents_tool::unknown_id(display))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                cancel_listed(self.services.as_ref(), &self.caller, &ids).await
            }
            super::agents_tool::AgentAction::List { ids } => self.list_agents(ids).await,
            super::agents_tool::AgentAction::Run { label, workflow } => {
                self.run_workflow(call, &label, workflow).await
            }
        }
    }

    async fn list_agents(&self, requested: Vec<String>) -> Result<String, ServiceError> {
        let agents = match self.services.agents(&self.caller, AgentsOp::List).await? {
            AgentsReply::Listed(agents) => agents,
            _ => {
                return Err(ServiceError::failed(
                    None,
                    "agents service returned an unexpected reply",
                ));
            }
        };
        let filtered = agents.iter().filter(|agent| {
            requested.is_empty() || requested.iter().any(|id| id == &agent.id.to_string())
        });
        let lines = filtered
            .map(|agent| format!("{} {} {:?}", agent.id, agent.name, agent.state))
            .collect::<Vec<_>>();
        Ok(if lines.is_empty() {
            "No child sessions.".to_owned()
        } else {
            lines.join("\n")
        })
    }

    async fn run_workflow(
        &mut self,
        call: CallId,
        label: &str,
        workflow: super::workflow::Workflow,
    ) -> Result<String, ServiceError> {
        let mut reports = Vec::new();
        for (step_index, step) in workflow.steps.iter().enumerate() {
            let from_items;
            let items = match &step.items {
                super::workflow::Items::Task => vec![None],
                super::workflow::Items::Literal(items) => items.iter().map(Some).collect(),
                super::workflow::Items::From(source) => {
                    let source = reports
                        .iter()
                        .find(|result: &&super::workflow::StepResult| result.name == *source);
                    let Some(source) = source else {
                        reports.push(super::workflow::StepResult {
                            name: step.name.clone(),
                            task_report: None,
                            pool_items: Some(Vec::new()),
                        });
                        continue;
                    };
                    let Some(text) = source.task_report.as_deref() else {
                        reports.push(super::workflow::StepResult {
                            name: step.name.clone(),
                            task_report: None,
                            pool_items: Some(Vec::new()),
                        });
                        continue;
                    };
                    let parts = super::pool::split_items(text);
                    if parts.len() > super::pool::ITEM_LINES_LIMIT {
                        return Err(ServiceError::failed(
                            None,
                            super::pool::too_many_items(source.name.as_str(), parts.len()),
                        ));
                    }
                    from_items = parts;
                    from_items.iter().map(Some).collect()
                }
            };
            let mut item_results = Vec::new();
            let mut task_report = None;
            for (item_index, item) in items.iter().enumerate() {
                let dependency_reports = reports.clone();
                let prompt = super::workflow::render::render(
                    step,
                    item.map(String::as_str),
                    &dependency_reports,
                    None,
                    JobId::new_v7(),
                );
                let task_name = item.map_or_else(
                    || step.name.clone(),
                    |item| super::pool::item_label(&step.name, item_index, item),
                );
                let preamble = super::pool::preamble(&task_name, &prompt);
                let mut tool_names = Vec::new();
                for tool in &step.tools {
                    tool_names.push(
                        Name::parse(tool)
                            .map_err(|error| ServiceError::failed(None, error.to_string()))?,
                    );
                }
                tool_names.push(
                    Name::parse("report")
                        .map_err(|error| ServiceError::failed(None, error.to_string()))?,
                );
                let child_call = CallId::new(format!("{call}-{}-{item_index}", step.name));
                let start = dal_core::AgentStart {
                    call: child_call,
                    name: task_name.clone().into_boxed_str(),
                    prompt: preamble.into_boxed_str(),
                    model: step.model.clone().map(String::into_boxed_str),
                    role: step.role.clone().map(String::into_boxed_str),
                    system: step.system.clone().map(String::into_boxed_str),
                    tools: Some(tool_names.into_boxed_slice()),
                    workspace: None,
                };
                let child = match self
                    .services
                    .agents(&self.caller, AgentsOp::Start(start))
                    .await?
                {
                    AgentsReply::Started { id } => id,
                    _ => {
                        return Err(ServiceError::failed(
                            None,
                            "agents service did not start the child",
                        ));
                    }
                };
                let report = await_child(self.services.as_ref(), &self.caller, child).await?;
                release_child(self.services.as_ref(), &self.caller, child).await;
                if item.is_none() {
                    task_report = Some(report.text.clone());
                }
                item_results.push(super::workflow::PoolItemResult {
                    item: item.map_or_else(
                        || step.name.clone().into_boxed_str(),
                        |item| item.clone().into_boxed_str(),
                    ),
                    state: "done".into(),
                    summary: super::delivery::preview(&report.text, 200).into_boxed_str(),
                });
                let result = super::pool::TaskResult {
                    id: JobId::new_v7(),
                    state: super::pool::TaskState::Done(super::agents_tool::Report {
                        status: ReportStatus::Done,
                        text: report.text.to_string(),
                    }),
                    changed: Vec::new(),
                    isolation: None,
                };
                let _ = result;
            }
            reports.push(super::workflow::StepResult {
                name: step.name.clone(),
                task_report,
                pool_items: match step.items {
                    super::workflow::Items::Task => None,
                    _ => Some(item_results),
                },
            });
            let _ = step_index;
        }
        let full = reports
            .iter()
            .filter_map(|report| report.task_report.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(format!("run {label} completed.\n\n{full}"))
    }

    async fn tool_call(&mut self, event: ToolCallEvent) -> ToolCallVerdict {
        let mut verdict = ToolCallVerdict::Allow;
        if self.config.loop_guard.enabled {
            match on_tool_call(&mut self.guard, event.tool.as_str(), &event.args) {
                Ok(effects) => {
                    if let Some(text) = effects.steer {
                        let _ = self
                            .services
                            .turn(&self.caller, TurnOp::Steer { text })
                            .await;
                    }
                    if let Some(text) = effects.warning {
                        self.services.notify(
                            &self.caller,
                            Notice {
                                turn: Some(event.turn),
                                kind: "orchestration.loop_guard".into(),
                                text,
                            },
                        );
                    }
                    if let Some(text) = effects.p1_recovery {
                        self.arbiter.admit_recovery(text.into(), Instant::now());
                    }
                    if let Some(reason) = effects.pause_reason {
                        self.arbiter.pause(reason);
                    }
                    if effects.cancel_turn {
                        let _ = self.services.turn(&self.caller, TurnOp::Cancel).await;
                    }
                    if let GuardVerdict::Block { reason } = effects.verdict {
                        verdict = ToolCallVerdict::Block { reason };
                    }
                }
                Err(error) => {
                    verdict = ToolCallVerdict::Block {
                        reason: format!("loop guard could not inspect tool arguments: {error}")
                            .into_boxed_str(),
                    };
                }
            }
        }
        if matches!(verdict, ToolCallVerdict::Allow)
            && self.config.sleep.enabled
            && event.tool.as_str() == "exec"
            && let Some(classifier) = self.sleep.as_ref()
        {
            let parsed = sonic_rs::from_str::<sonic_rs::Value>(event.args.as_str());
            if let Ok(args) = parsed
                && let Some(command) = args
                    .as_object()
                    .and_then(|object| object.get(&"command"))
                    .and_then(sonic_rs::JsonValueTrait::as_str)
                && let Some(wait) = classifier.classify(command)
                && let Ok(Some(args)) = rewrite_exec_args(&event.args, wait)
            {
                verdict = ToolCallVerdict::Rewrite { args };
            }
        }
        verdict
    }

    fn publish_status(&self) {
        let inflight = InflightCounts {
            jobs: self.inflight_jobs,
            monitors: self.monitors.live_count(),
            asks: self.open_asks.len(),
            goal_timer: u8::from(self.goal_timer.is_some()),
            loop_guard: u8::from(self.guard.episode.is_some()),
        };
        let others = inflight.jobs + inflight.monitors + usize::from(inflight.goal_timer > 0);
        let quiet = self.arbiter.quiet(true, others, 0, Instant::now());
        let goal = self.goal.as_ref().and_then(adapter::preview);
        let payload = status_payload(self.arbiter.mode(), quiet, inflight, 0, goal);
        if let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.quiet = quiet;
            snapshot.text = Some(status_json(&payload).into_boxed_str());
        }
    }
}

struct SessionJobsView<'a> {
    jobs: &'a [dal_core::JobStatus],
}

impl JobsView for SessionJobsView<'_> {
    fn resolve_job(&self, display: &str) -> Option<JobId> {
        let id = JobId::parse(display).ok()?;
        self.jobs.iter().any(|job| job.id == id).then_some(id)
    }

    fn is_live_top_level_exec(&self, id: JobId) -> bool {
        self.jobs.iter().any(|job| {
            job.id == id && matches!(job.state, JobStateView::Running | JobStateView::Detached)
        })
    }

    fn top_level_live_count(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| matches!(job.state, JobStateView::Running | JobStateView::Detached))
            .count()
    }
}

fn failed(message: &str) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

/// The result of cancelling every active descendant of one session.
#[derive(Default)]
struct Sweep {
    failures: Vec<String>,
}

impl Sweep {
    /// The failures joined for a reply or notice, or `None` when every
    /// cancel worked.
    fn failure_text(&self) -> Option<String> {
        if self.failures.is_empty() {
            None
        } else {
            Some(self.failures.join("; "))
        }
    }
}

/// Cancels every queued or running session the host lists for `caller`.
/// The host may reveal deeper descendants while the first ones close, so
/// the list is read again until no unvisited session is left. A visited set
/// keeps each session to one cancel, and a failure never stops the walk:
/// it is recorded next to the session it belongs to.
async fn cancel_descendants(services: &dyn Services, caller: &Caller) -> Sweep {
    let mut sweep = Sweep::default();
    let mut visited = HashSet::new();
    loop {
        let listed = match services.agents(caller, AgentsOp::List).await {
            Ok(AgentsReply::Listed(agents)) => agents,
            Ok(_) => {
                sweep.failures.push(
                    "the agents service returned an unexpected reply to a list request".to_owned(),
                );
                return sweep;
            }
            Err(error) => {
                sweep
                    .failures
                    .push(format!("the child sessions could not be listed: {error}"));
                return sweep;
            }
        };
        let pending: Vec<SessionId> = listed
            .iter()
            .filter(|agent| matches!(agent.state, AgentState::Queued | AgentState::Running))
            .map(|agent| agent.id)
            .filter(|id| visited.insert(*id))
            .collect();
        if pending.is_empty() {
            return sweep;
        }
        for id in pending {
            match services.agents(caller, AgentsOp::Cancel { id }).await {
                Ok(AgentsReply::Cancelled { .. }) => {}
                Ok(_) => sweep.failures.push(format!(
                    "{id}: the agents service returned an unexpected reply to a cancel request"
                )),
                Err(error) => sweep.failures.push(format!("{id}: {error}")),
            }
        }
    }
}

/// Cancels the sessions the model named. A session the host refuses to
/// cancel does not stop the others: every id is tried once, and the refusals
/// are reported together with the count that did close.
async fn cancel_listed(
    services: &dyn Services,
    caller: &Caller,
    ids: &[SessionId],
) -> Result<String, ServiceError> {
    let mut seen = HashSet::new();
    let mut cancelled = 0;
    let mut failures = Vec::new();
    for &id in ids.iter().filter(|id| seen.insert(**id)) {
        match services.agents(caller, AgentsOp::Cancel { id }).await {
            Ok(AgentsReply::Cancelled { .. }) => cancelled += 1,
            Ok(_) => {}
            Err(error) => failures.push(format!("{id}: {error}")),
        }
    }
    if failures.is_empty() {
        return Ok(format!("cancelled {cancelled} child sessions."));
    }
    Err(ServiceError::failed(
        None,
        format!(
            "cancelled {cancelled} child sessions; could not cancel {}.",
            failures.join("; ")
        ),
    ))
}

/// Waits for one child's report. When the wait fails, the child is closed
/// so it cannot keep running unseen, and a failed close stays next to the
/// wait failure instead of replacing it.
async fn await_child(
    services: &dyn Services,
    caller: &Caller,
    child: SessionId,
) -> Result<AgentReport, ServiceError> {
    let failure = match services
        .agents(
            caller,
            AgentsOp::Await {
                id: child,
                timeout: None,
            },
        )
        .await
    {
        Ok(AgentsReply::Await { report }) => return Ok(report),
        Ok(_) => ServiceError::failed(None, "agents service did not return the child report"),
        Err(error) => error,
    };
    match services
        .agents(caller, AgentsOp::Cancel { id: child })
        .await
    {
        Ok(_) => Err(failure),
        Err(teardown) => Err(ServiceError::failed(
            None,
            super::pool::failure_with_teardown(&failure.to_string(), &teardown.to_string()),
        )),
    }
}

/// Closes a run child whose report has been taken. The run owns the child
/// and never addresses it again, so nothing else would close it: with no
/// client attached, no front end would either, and every finished child
/// would keep its runtime until the whole session ended. The close is
/// awaited before the run starts its next child, so a new child never
/// begins while an earlier release is still pending. A refused close does
/// not fail the run, because the report is already in hand; it is reported
/// as a notice instead.
async fn release_child(services: &dyn Services, caller: &Caller, child: SessionId) {
    let failure = match services
        .agents(caller, AgentsOp::Cancel { id: child })
        .await
    {
        Ok(AgentsReply::Cancelled { .. }) => return,
        Ok(_) => "the agents service returned an unexpected reply to a close request".to_owned(),
        Err(error) => error.to_string(),
    };
    services.notify(
        caller,
        Notice {
            turn: None,
            kind: "orchestration.release".into(),
            text: format!(
                "The finished child session {child} was not closed: {failure}. Its report is kept, but it may still hold memory until this session ends."
            )
            .into(),
        },
    );
}

pub(crate) struct SessionStartHook(pub(crate) Runtime);

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, start: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        Box::pin(async move { runtime.open(start, cx).await })
    }
}

pub(crate) struct SessionEndHook(pub(crate) Runtime);

impl ObserveHook<SessionEnd> for SessionEndHook {
    fn call(&self, end: SessionEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        Box::pin(async move {
            runtime.close(cx.session, end).await;
            Ok(())
        })
    }
}

pub(crate) struct InputHook(pub(crate) Runtime);

impl Hook<InputEvent, InputVerdict> for InputHook {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            match runtime
                .hook_request(session, deadline, cancel, Message::Input)
                .await?
            {
                Ok(()) => Ok(InputVerdict::Continue),
                Err(error) => Err(failed(&error.to_string())),
            }
        })
    }
}

pub(crate) struct ToolCallHook(pub(crate) Runtime);

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolCallHook {
    fn call(
        &self,
        event: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::ToolCall(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct ToolResultHook(pub(crate) Runtime);

impl ObserveHook<ToolResultEvent> for ToolResultHook {
    fn call(
        &self,
        event: ToolResultEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::ToolResult(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct TurnEndHook(pub(crate) Runtime);

impl ObserveHook<TurnEnd> for TurnEndHook {
    fn call(&self, event: TurnEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::TurnEnd(event, reply)
                })
                .await
        })
    }
}

pub(crate) struct SettledHook(pub(crate) Runtime);

impl ObserveHook<Settled> for SettledHook {
    fn call(&self, event: Settled, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let runtime = self.0.clone();
        let session = cx.session;
        let deadline = cx.deadline;
        let cancel = cx.cancel;
        Box::pin(async move {
            match runtime
                .hook_request(session, deadline, cancel, |reply| {
                    Message::Settled(event, reply)
                })
                .await?
            {
                Ok(()) => Ok(()),
                Err(error) => Err(failed(&error.to_string())),
            }
        })
    }
}

pub(crate) struct Status(pub(crate) Runtime);

impl StatusPoll for Status {
    fn snapshot(&self, cx: &StatusCx) -> StatusSnapshot {
        self.0.snapshot(cx.session).unwrap_or(StatusSnapshot {
            quiet: true,
            text: None,
        })
    }
}

fn stop_kind(stop: &dal_core::Stop) -> StopKind {
    match stop {
        dal_core::Stop::EndTurn => StopKind::Completed,
        dal_core::Stop::Length | dal_core::Stop::MaxSteps => StopKind::Length,
        dal_core::Stop::Filter => StopKind::Filter,
        dal_core::Stop::Cancelled => StopKind::Cancelled,
        dal_core::Stop::Failed => StopKind::Error,
    }
}
