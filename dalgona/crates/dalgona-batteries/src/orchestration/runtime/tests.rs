// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Cancellation and settlement of child sessions, driven through the real
//! session owner against a scripted host.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, HookCx, RawValue, Services, Tool, ToolCx, ToolOutcome,
};
use dal_core::ext::{
    McpDeclaration, McpRequest, McpResponse, SessionEnd, SessionStart, Visibility,
};
use dal_core::{
    AgentInfo, AgentReport, AgentState, AgentsOp, AgentsReply, Answer, CallId, EntryId,
    FetchRequest, FetchResponse, Inference, JobsOp, JobsReply, ModelRequest, Notice, Question,
    RawJson, RunOutput, RunRequest, SessionId, SidecarOp, Stop, TurnOp, TurnOpReply,
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
    report: Option<&'static str>,
    cancels: Vec<SessionId>,
    notices: Vec<Notice>,
}

/// A host that answers only the services a session owner uses when it
/// cancels children; every other service reports itself unavailable.
#[derive(Default)]
struct Host {
    script: Mutex<Script>,
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

    fn notices(&self) -> Vec<Notice> {
        locked(&self.script).notices.clone()
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

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable()
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

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
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
            AgentsOp::Start(_) => match script.started {
                Some(id) => Ok(AgentsReply::Started { id }),
                None => Err(ServiceError::failed(None, "no child was scripted")),
            },
            AgentsOp::Await { id, .. } => match (script.await_error, script.report) {
                (Some(message), _) => Err(ServiceError::failed(None, message)),
                (None, Some(text)) => Ok(AgentsReply::Await {
                    report: AgentReport {
                        stop: Stop::EndTurn,
                        text: text.into(),
                        session: id,
                        entry: EntryId::new(std::num::NonZeroU64::MIN),
                    },
                }),
                (None, None) => Err(ServiceError::failed(None, "no report was scripted")),
            },
            _ => Err(ServiceError::failed(
                None,
                "unavailable in the scripted host",
            )),
        };
        Box::pin(async move { reply })
    }

    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        match op {
            JobsOp::List => {
                let mut script = locked(&self.script);
                let reply = match script.jobs_list_error.take() {
                    Some(message) => Err(ServiceError::failed(None, message)),
                    None => Ok(JobsReply::Listed(Vec::new())),
                };
                Box::pin(async move { reply })
            }
            _ => unavailable(),
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
            _ => unavailable(),
        }
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
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
    async fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = parse_config(None)?;
        config.goal.enabled = false;
        let runtime = Runtime::new(config)?;
        let host = Arc::new(Host::default());
        let session = SessionId::new_v7();
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
        Ok(Self {
            runtime,
            host,
            session,
        })
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        locked(&self.host.script)
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
    fixture
        .runtime
        .tool(
            fixture.session,
            CallId::new("call"),
            "agents",
            RawJson::parse(
                "{\"action\":\"run\",\"steps\":[{\"name\":\"scan\",\"prompt\":\"look\"}]}",
            )
            .map_err(|error| ServiceError::failed(None, error.to_string()))?,
            CancellationToken::new(),
        )
        .await
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
    let error = run_one_step(&fixture)
        .await
        .err()
        .ok_or("the run must fail")?
        .to_string();
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
    let error = run_one_step(&fixture)
        .await
        .err()
        .ok_or("the run must fail")?
        .to_string();
    assert_eq!(fixture.host.cancels(), vec![child]);
    assert!(error.contains("the provider stopped answering"), "{error}");
    assert!(!error.contains(CLOSE_REFUSED), "{error}");
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
        script.report = Some("scan finished");
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
        script.report = Some("scan finished");
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
        error.contains("cancelled 1 child sessions")
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
