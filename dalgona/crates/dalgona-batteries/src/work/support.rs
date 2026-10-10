// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dal_agent::error::{SchemeError, ServiceError};
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, Hook, HookCx, ObserveHook, RawValue, Services, Tool, ToolCall,
    ToolCx, ToolOutcome,
};
use dal_core::ext::{
    BeforeTurn, McpDeclaration, McpRequest, McpResponse, SessionEnd, SessionStart, StateError,
    StateOp, StateRecord, ToolCallEvent, ToolCallVerdict, Visibility,
};
use dal_core::{
    AgentReport, AgentStart, AgentsOp, AgentsReply, Answer, CallId, EntryId, FetchRequest,
    FetchResponse, Inference, JobId, JobsOp, JobsReply, ModelRequest, Name, Notice, Question,
    RawJson, RunOutput, RunRequest, Service, SessionId, SidecarOp, Stop, ToolClass, TurnId, TurnOp,
    TurnOpReply,
};

use super::plan::{self, BatteryState, Host};
use super::{PlanConfig, todo};

use unicode_segmentation::UnicodeSegmentation;

fn turn() -> TurnId {
    TurnId::new(NonZeroU64::MIN)
}

pub(crate) type TestResult = Result<(), Box<dyn StdError>>;

pub(crate) enum Scripted {
    Label(&'static str),
    Dismissed,
    Held,
}

struct Entry {
    parent: Option<usize>,
    kind: Box<str>,
    body: RawJson,
}

#[derive(Default)]
struct Ledger {
    entries: Vec<Entry>,
    leaf: Option<usize>,
}

#[derive(Default)]
pub(crate) struct FakeServices {
    ledger: Mutex<Ledger>,
    script: Mutex<VecDeque<Scripted>>,
    questions: Mutex<Vec<Question>>,
    append_failures: Mutex<VecDeque<bool>>,
    agent_starts: Mutex<Vec<AgentStart>>,
    asked: tokio::sync::Notify,
    cancel: tokio::sync::Notify,
    side_calls: AtomicUsize,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Mirrors the store's session-name admission rule (`dal-store`'s
/// `normalize_name`): trims, collapses line-break runs to one space,
/// then 1-64 grapheme clusters, no control characters, and at least
/// one character other than `0-9`, `a-f`, and `-`.
fn name_admitted(name: &str) -> bool {
    let mut out = String::new();
    let mut breaking = false;
    for ch in name.trim().chars() {
        if ch == '\r' || ch == '\n' {
            if !breaking {
                out.push(' ');
                breaking = true;
            }
        } else {
            breaking = false;
            out.push(ch);
        }
    }
    let id_only = !out.is_empty()
        && out
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f' | b'-'));
    (1..=64).contains(&out.graphemes(true).count())
        && !out.chars().any(char::is_control)
        && !id_only
}

fn unavailable<T: Send + 'static>(calls: &AtomicUsize) -> ServiceFuture<'static, T> {
    calls.fetch_add(1, Ordering::SeqCst);
    Box::pin(async {
        Err(ServiceError::failed(
            None,
            "unavailable in the scripted host",
        ))
    })
}

impl FakeServices {
    pub(crate) fn script(&self, answers: impl IntoIterator<Item = Scripted>) {
        locked(&self.script).extend(answers);
    }

    pub(crate) fn fail_appends(&self, pattern: impl IntoIterator<Item = bool>) {
        locked(&self.append_failures).extend(pattern);
    }

    pub(crate) fn asked_count(&self) -> usize {
        locked(&self.questions).len()
    }

    pub(crate) fn question(&self, index: usize) -> Option<Question> {
        locked(&self.questions).get(index).cloned()
    }

    pub(crate) fn side_calls(&self) -> usize {
        self.side_calls.load(Ordering::SeqCst)
    }

    /// The child names every scripted `agents.start` received, in order.
    pub(crate) fn agent_start_names(&self) -> Vec<String> {
        locked(&self.agent_starts)
            .iter()
            .map(|start| start.name.to_string())
            .collect()
    }

    pub(crate) async fn wait_asked(&self) -> bool {
        tokio::time::timeout(Duration::from_secs(5), self.asked.notified())
            .await
            .is_ok()
    }

    pub(crate) fn cancel_ask(&self) {
        self.cancel.notify_one();
    }

    pub(crate) fn set_leaf(&self, leaf: Option<usize>) {
        locked(&self.ledger).leaf = leaf;
    }

    pub(crate) fn push_raw(&self, kind: &str, body: &str) -> Result<(), Box<dyn StdError>> {
        let body = RawJson::parse(body)?;
        let mut ledger = locked(&self.ledger);
        let index = ledger.entries.len();
        let parent = ledger.leaf;
        ledger.entries.push(Entry {
            parent,
            kind: kind.into(),
            body,
        });
        ledger.leaf = Some(index);
        Ok(())
    }

    pub(crate) fn all_bodies(&self, kind: &str) -> Vec<String> {
        locked(&self.ledger)
            .entries
            .iter()
            .filter(|entry| &*entry.kind == kind)
            .map(|entry| entry.body.as_str().to_owned())
            .collect()
    }

    #[expect(
        clippy::vec_box,
        reason = "records() must yield Vec<Box<RawValue>> per the Services contract"
    )]
    pub(crate) fn leaf_bodies(&self, kind: &str) -> Vec<Box<RawJson>> {
        let ledger = locked(&self.ledger);
        let mut bodies = Vec::new();
        let mut cursor = ledger.leaf;
        while let Some(index) = cursor {
            let entry = &ledger.entries[index];
            if &*entry.kind == kind {
                bodies.push(Box::new(entry.body.clone()));
            }
            cursor = entry.parent;
        }
        bodies.reverse();
        bodies
    }
}

impl Services for FakeServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable(&self.side_calls)
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable(&self.side_calls)
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable(&self.side_calls)
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable(&self.side_calls)
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable(&self.side_calls)
    }

    fn ask(&self, _who: &Caller, question: Question) -> ServiceFuture<'_, Option<Answer>> {
        locked(&self.questions).push(question);
        let next = locked(&self.script).pop_front();
        self.asked.notify_one();
        Box::pin(async move {
            match next {
                Some(Scripted::Label(text)) => {
                    let encoded = sonic_rs::to_string(text)
                        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                    let value = RawJson::parse(&encoded)
                        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                    Ok(Some(Answer::Value(value)))
                }
                Some(Scripted::Dismissed) | None => Ok(None),
                Some(Scripted::Held) => {
                    self.cancel.notified().await;
                    Err(ServiceError::Cancelled)
                }
            }
        })
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable(&self.side_calls)
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable(&self.side_calls)
    }

    fn agents(&self, _who: &Caller, op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        match op {
            AgentsOp::Start(start) => {
                let name = start.name.clone();
                locked(&self.agent_starts).push(start);
                // Mirror the store's session-name admission rule so a child
                // name that the real host would refuse fails here too.
                if !name_admitted(&name) {
                    return Box::pin(async {
                        Err(ServiceError::failed(
                            Some(Service::Agents),
                            "a session name must have 1 to 64 characters, no control characters, and at least one character other than 0-9, a-f, and -",
                        ))
                    });
                }
                Box::pin(async {
                    Ok(AgentsReply::Started {
                        id: SessionId::new_v7(),
                    })
                })
            }
            AgentsOp::Await { id, .. } => Box::pin(async move {
                Ok(AgentsReply::Await {
                    report: AgentReport {
                        stop: Stop::EndTurn,
                        text: "done".into(),
                        session: id,
                        entry: EntryId::new(NonZeroU64::MIN),
                    },
                })
            }),
            _ => unavailable(&self.side_calls),
        }
    }

    fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        if let JobsOp::Spawn { .. } = op {
            return Box::pin(async {
                Ok(JobsReply::Spawned {
                    id: JobId::new_v7(),
                })
            });
        }
        unavailable(&self.side_calls)
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable(&self.side_calls)
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unavailable(&self.side_calls)
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable(&self.side_calls)
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable(&self.side_calls)
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
        unavailable(&self.side_calls)
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unavailable(&self.side_calls)
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable(&self.side_calls)
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable(&self.side_calls)
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable(&self.side_calls)
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unavailable(&self.side_calls)
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unavailable(&self.side_calls)
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable(&self.side_calls)
    }

    fn notify(&self, _who: &Caller, _notice: Notice) {
        self.side_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn append_record(
        &self,
        _who: &Caller,
        kind: &str,
        body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        let failing = locked(&self.append_failures).pop_front().unwrap_or(false);
        if failing {
            return Box::pin(async { Err(ServiceError::failed(None, "the journal write failed")) });
        }
        let mut ledger = locked(&self.ledger);
        let index = ledger.entries.len();
        let parent = ledger.leaf;
        ledger.entries.push(Entry {
            parent,
            kind: kind.into(),
            body: *body,
        });
        ledger.leaf = Some(index);
        let id = u64::try_from(index)
            .ok()
            .and_then(|index| NonZeroU64::new(index.saturating_add(1)))
            .unwrap_or(NonZeroU64::MIN);
        Box::pin(async move { Ok(EntryId::new(id)) })
    }

    fn records(&self, _who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let bodies = self.leaf_bodies(kind);
        Box::pin(async move { Ok(bodies) })
    }
}

pub(crate) fn outcome_text(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Ok(output) => output.to_string(),
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Interrupted => "interrupted".to_owned(),
        ToolOutcome::Detached(_) => "detached".to_owned(),
    }
}

pub(crate) struct ScriptedWorkHost {
    pub(crate) services: Arc<FakeServices>,
    pub(crate) state: Arc<BatteryState>,
    pub(crate) session: SessionId,
    caller: Caller,
}

impl ScriptedWorkHost {
    pub(crate) fn open() -> Self {
        let services = Arc::new(FakeServices::default());
        let session = SessionId::new_v7();
        let dynamic: Arc<dyn Services> = services.clone();
        let caller = HookCx::for_test(dynamic, session, None).caller;
        Self {
            services,
            state: Arc::new(BatteryState::default()),
            session,
            caller,
        }
    }

    fn hook_cx(&self) -> HookCx {
        let services: Arc<dyn Services> = self.services.clone();
        HookCx::for_test(services, self.session, Some(turn()))
    }

    pub(crate) fn host(&self) -> Host<'_> {
        Host {
            services: self.services.as_ref(),
            caller: &self.caller,
            session: self.session,
        }
    }

    pub(crate) async fn tool(&self, name: &str, args: &str) -> Result<String, Box<dyn StdError>> {
        let extension = super::work(PlanConfig { enabled: true })?;
        let tool = extension
            .tools()
            .iter()
            .find(|(tool, _)| tool.name().as_str() == name)
            .map(|(tool, _)| Arc::clone(tool))
            .ok_or("tool is not registered")?;
        let services: Arc<dyn Services> = self.services.clone();
        let cx = ToolCx::for_test(services);
        let call = ToolCall::new("call", RawJson::parse(args)?);
        Ok(outcome_text(tool.run(call, cx).await))
    }

    pub(crate) async fn plan(&self, args: &str) -> String {
        match plan::submit(&self.state, args, self.host()).await {
            Ok(text) => text,
            Err(outcome) => outcome_text(outcome),
        }
    }

    pub(crate) async fn todo_tool(&self, args: &str) -> String {
        match todo::tool(args, self.services.as_ref(), &self.caller).await {
            Ok(text) => text,
            Err(outcome) => outcome_text(outcome),
        }
    }

    pub(crate) fn plan_command(&self, args: &str) -> String {
        match plan::command(&self.state, self.session, args) {
            Ok(effect) => {
                if effect.cancel_turn {
                    self.services.cancel_ask();
                }
                effect.text.to_owned()
            }
            Err(error) => error.to_string(),
        }
    }

    pub(crate) async fn todos_command(&self, args: &str) -> Result<String, ServiceError> {
        super::todos_reply(args, self.services.as_ref(), &self.caller).await
    }

    pub(crate) async fn scheme(&self, path: &str) -> Result<String, SchemeError> {
        super::resolve_scheme(path, self.services.as_ref(), &self.caller)
            .await
            .map(|doc| doc.text.into())
    }

    pub(crate) fn status(&self) -> (bool, Option<String>) {
        let bodies = self.services.leaf_bodies(todo::TODO_KIND);
        let items = todo::fold(bodies.as_slice());
        let snapshot = super::status_snapshot(self.state.phase(self.session), &items);
        (snapshot.quiet, snapshot.text.map(Into::into))
    }

    pub(crate) async fn guard(
        &self,
        tool: &str,
        class: ToolClass,
    ) -> Result<Option<String>, Box<dyn StdError>> {
        let event = ToolCallEvent {
            turn: turn(),
            call: CallId::new("call"),
            tool: Name::parse(tool)?,
            class,
            args: RawJson::parse("{}")?,
        };
        let hook = super::ToolGuard {
            state: Arc::clone(&self.state),
        };
        match hook.call(event, self.hook_cx()).await? {
            ToolCallVerdict::Block { reason } => Ok(Some(reason.into())),
            _ => Ok(None),
        }
    }

    pub(crate) async fn before_turn(&self) -> Result<Option<String>, Box<dyn StdError>> {
        let hook = super::TurnContext {
            state: Arc::clone(&self.state),
        };
        let event = BeforeTurn {
            turn: turn(),
            text: "hello".into(),
        };
        Ok(hook.call(event, self.hook_cx()).await?)
    }

    pub(crate) async fn open_session(&self) -> TestResult {
        let workspace = ToolCx::for_test(self.services.clone()).workspace().clone();
        let hook = super::SessionOpen {
            state: Arc::clone(&self.state),
        };
        let event = SessionStart {
            session: self.session,
            workspace,
            resumed: true,
        };
        Ok(hook.call(event, self.hook_cx()).await?)
    }

    pub(crate) async fn close_session(&self) -> TestResult {
        let hook = super::SessionClose {
            state: Arc::clone(&self.state),
        };
        let event = SessionEnd {
            session: self.session,
            reason: "closed".into(),
        };
        Ok(hook.call(event, self.hook_cx()).await?)
    }
}
