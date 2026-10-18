// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! A scripted `Services` host for driving one battery tool through its public extension.

#![expect(clippy::expect_used, reason = "test assertions abort on failure")]

use std::collections::{HashMap, VecDeque};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, Doc, EventStream, Extension, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{
    McpDeclaration, McpRequest, McpResponse, StateError, StateOp, StateRecord, Visibility,
};
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
#[derive(Clone, Default)]
pub(crate) enum Net {
    #[default]
    Unavailable,
    Hang,
    Cancel,
    /// Fails every request with a service error carrying `cause`.
    Fail {
        cause: String,
    },
    /// Answers each request with the next response of `Host::net_script`.
    Script,
    /// Sends each request over a real socket, as the host backend does: no
    /// redirect is followed and a send failure answers status 0 with no body.
    /// `base` rewrites the scheme and authority of every URL, which stands in
    /// for the host's egress to a fixed public endpoint.
    Live {
        base: Option<String>,
    },
}

#[derive(Default)]
pub(crate) struct Host {
    pub(crate) answers: Mutex<VecDeque<Reply>>,
    pub(crate) questions: Mutex<Vec<Question>>,
    pub(crate) runs: Mutex<VecDeque<RunOutput>>,
    pub(crate) infer_error: Mutex<Option<String>>,
    pub(crate) infer_calls: AtomicUsize,
    pub(crate) net: Mutex<Net>,
    /// Responses a `Net::Script` host answers with, in order.
    pub(crate) net_script: Mutex<VecDeque<FetchResponse>>,
    /// Every request that reached the `net` service, in order.
    pub(crate) net_requests: Mutex<Vec<FetchRequest>>,
    /// Environment values the `env` service answers with; an absent key is
    /// unset, so `env()` returns `Ok(None)`.
    pub(crate) env_vars: Mutex<HashMap<String, String>>,
    pub(crate) records: Mutex<HashMap<String, Vec<RawJson>>>,
    pub(crate) files: Mutex<HashMap<String, Vec<u8>>>,
    /// Notices the tools published, in order (ask malformed-answer notes).
    pub(crate) notices: Mutex<Vec<Notice>>,
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

    fn net(&self, _who: &Caller, req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        locked(&self.net_requests).push(req.clone());
        match locked(&self.net).clone() {
            Net::Unavailable => unavailable(),
            Net::Hang => Box::pin(std::future::pending()),
            Net::Cancel => Box::pin(async { Err(ServiceError::Cancelled) }),
            Net::Fail { cause } => Box::pin(async move { Err(ServiceError::failed(None, cause)) }),
            Net::Script => {
                let next = locked(&self.net_script).pop_front();
                Box::pin(async move {
                    next.ok_or_else(|| ServiceError::failed(None, "the net script is exhausted"))
                })
            }
            Net::Live { base } => Box::pin(live_fetch(req, base)),
        }
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        let next = locked(&self.runs).pop_front();
        Box::pin(
            async move { next.ok_or_else(|| ServiceError::failed(None, "no scripted git output")) },
        )
    }

    fn env(&self, _who: &Caller, key: &str) -> ServiceFuture<'_, Option<String>> {
        let value = locked(&self.env_vars).get(key).cloned();
        Box::pin(async move { Ok(value) })
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

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
        unavailable()
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

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable()
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn model_ready(
        &self,
        _who: &Caller,
        model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        Box::pin(async move { Ok(Some(model)) })
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

    fn notify(&self, _who: &Caller, notice: Notice) {
        locked(&self.notices).push(notice);
    }

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

/// Sends one request over a real socket and folds the reply into a response.
/// A send failure answers status 0 with no body, as the host backend does.
async fn live_fetch(
    req: FetchRequest,
    base: Option<String>,
) -> Result<FetchResponse, ServiceError> {
    let failure = FetchResponse {
        status: 0,
        headers: Vec::new(),
        body: Vec::new(),
    };
    let mut target = req.url.to_string();
    if let Some(base) = base {
        let parsed = reqwest::Url::parse(&req.url)
            .map_err(|error| ServiceError::failed(None, error.to_string()))?;
        target = format!("{base}{}", parsed.path());
        if let Some(query) = parsed.query() {
            target.push('?');
            target.push_str(query);
        }
    }
    let method = match req.method {
        dal_core::ext::FetchMethod::Post => reqwest::Method::POST,
        _ => reqwest::Method::GET,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    let mut outgoing = client.request(method, target).body(req.body);
    for (name, value) in req.headers {
        outgoing = outgoing.header(name.as_ref(), value.as_ref());
    }
    let Ok(response) = outgoing.send().await else {
        return Ok(failure);
    };
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().into(), value.to_str().unwrap_or("").into()))
        .collect();
    let Ok(body) = response.bytes().await else {
        return Ok(failure);
    };
    Ok(FetchResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

/// One request a [`serve`] listener received.
pub(crate) struct Received {
    pub(crate) method: String,
    /// The request target: path and query.
    pub(crate) target: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Received {
    /// The first header with a case-insensitive name match.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }
}

/// Accepts one HTTP connection on `listener`, records the request, and
/// answers `status` with `body` as `content_type`.
pub(crate) async fn serve(
    listener: &tokio::net::TcpListener,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Received {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut stream, _) = listener.accept().await.expect("accept a connection");
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk).await.expect("read the request");
        assert!(read > 0, "the client closed before sending a request head");
        raw.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut request_body = raw[head_end..].to_vec();
    while request_body.len() < length {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("read the request body");
        assert!(read > 0, "the client closed before sending its body");
        request_body.extend_from_slice(&chunk[..read]);
    }
    let reply = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(reply.as_bytes())
        .await
        .expect("write the head");
    stream.write_all(body).await.expect("write the body");
    stream.shutdown().await.expect("close the connection");
    Received {
        method,
        target,
        headers,
        body: request_body,
    }
}
