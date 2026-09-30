// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! A scripted `Services` host for driving one battery tool through its public extension.

use std::collections::{HashMap, VecDeque};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, Extension, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse, Visibility};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, Question, RawJson, RunOutput, RunRequest, SidecarOp, TurnOp,
    TurnOpReply,
};

pub(crate) type TestResult = Result<(), Box<dyn StdError>>;

/// One scripted answer of the ask controller.
pub(crate) enum Reply {
    Value(&'static str),
    Dismissed,
    Failed,
    Cancelled,
}

/// How the `net` service behaves.
#[derive(Clone, Copy, Default)]
pub(crate) enum Net {
    #[default]
    Unavailable,
    Hang,
    Cancel,
}

#[derive(Default)]
pub(crate) struct Host {
    pub(crate) answers: Mutex<VecDeque<Reply>>,
    pub(crate) questions: Mutex<Vec<Question>>,
    pub(crate) runs: Mutex<VecDeque<RunOutput>>,
    pub(crate) infer_error: Mutex<Option<String>>,
    pub(crate) infer_calls: AtomicUsize,
    pub(crate) net: Mutex<Net>,
    pub(crate) records: Mutex<HashMap<String, Vec<RawJson>>>,
    pub(crate) files: Mutex<HashMap<String, Vec<u8>>>,
}

pub(crate) fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
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
    pub(crate) fn answering(script: impl IntoIterator<Item = Reply>) -> Arc<Self> {
        let host = Self::default();
        locked(&host.answers).extend(script);
        Arc::new(host)
    }

    pub(crate) fn with_runs(runs: impl IntoIterator<Item = RunOutput>) -> Arc<Self> {
        let host = Self::default();
        locked(&host.runs).extend(runs);
        Arc::new(host)
    }

    pub(crate) fn asked(&self) -> Vec<Question> {
        locked(&self.questions).clone()
    }
}

pub(crate) fn exited(code: i32, stdout: &str, stderr: &str) -> RunOutput {
    RunOutput {
        status: dal_core::ExitStatusKind::Exited(code),
        stdout_tail: Vec::new(),
        stdout_prefix: stdout.as_bytes().to_vec(),
        stdout_prefix_overflowed: false,
        stderr_tail: stderr.as_bytes().to_vec(),
        log: None,
    }
}

impl Services for Host {
    fn fs_read(&self, _who: &Caller, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        let bytes = locked(&self.files).get(path).cloned();
        Box::pin(async move { Ok(bytes) })
    }

    fn fs_write(&self, _who: &Caller, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        let path = path.to_owned();
        locked(&self.files).insert(path, bytes);
        Box::pin(async { Ok(()) })
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        match *locked(&self.net) {
            Net::Unavailable => unavailable(),
            Net::Hang => Box::pin(std::future::pending()),
            Net::Cancel => Box::pin(async { Err(ServiceError::Cancelled) }),
        }
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        let next = locked(&self.runs).pop_front();
        Box::pin(
            async move { next.ok_or_else(|| ServiceError::failed(None, "no scripted git output")) },
        )
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unavailable()
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn ask(&self, _who: &Caller, question: Question) -> ServiceFuture<'_, Option<Answer>> {
        locked(&self.questions).push(question);
        let next = locked(&self.answers).pop_front();
        Box::pin(async move {
            match next {
                Some(Reply::Value(json)) => {
                    let value = RawJson::parse(json)
                        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
                    Ok(Some(Answer::Value(value)))
                }
                Some(Reply::Dismissed) | None => Ok(None),
                Some(Reply::Failed) => Err(ServiceError::failed(None, "the controller failed")),
                Some(Reply::Cancelled) => Err(ServiceError::Cancelled),
            }
        })
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable()
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unavailable()
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unavailable()
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
        Box::pin(async { Ok(None) })
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable()
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        self.infer_calls.fetch_add(1, Ordering::SeqCst);
        let message = locked(&self.infer_error).clone();
        Box::pin(async move {
            Err(ServiceError::failed(
                None,
                message.unwrap_or_else(|| "no scripted reply".to_owned()),
            ))
        })
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

    fn notify(&self, _who: &Caller, _notice: Notice) {}

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unavailable()
    }

    fn records(&self, _who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let bodies = locked(&self.records).get(kind).cloned().unwrap_or_default();
        Box::pin(async move { Ok(bodies.into_iter().map(Box::new).collect()) })
    }
}

/// Runs the tool `name` of `extension` once against `host` and renders its outcome as text.
pub(crate) async fn run_tool(
    extension: &Extension,
    host: &Arc<Host>,
    name: &str,
    args: &str,
) -> Result<String, Box<dyn StdError>> {
    let tool = extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == name)
        .map(|(tool, _)| Arc::clone(tool))
        .ok_or("the tool is not registered")?;
    let services: Arc<dyn Services> = host.clone();
    let call = ToolCall::new("call", RawJson::parse(args)?);
    Ok(match tool.run(call, ToolCx::for_test(services)).await {
        ToolOutcome::Ok(output) => output.to_string(),
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Interrupted => "interrupted".to_owned(),
        ToolOutcome::Detached(_) => "detached".to_owned(),
    })
}
