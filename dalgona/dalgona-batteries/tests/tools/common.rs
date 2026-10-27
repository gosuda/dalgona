// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! A scripted `Services` host for driving one battery tool through its public extension.

use std::collections::{HashMap, VecDeque};
use std::error::Error as StdError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

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
    let mut outgoing = client.request(method, &target).body(req.body);
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

/// The user agent prefix every battery request carries on the wire. The
/// production tools stamp it on each request, so a listener attributes its
/// requests by this mark before trusting one.
const CLIENT_MARK: &str = "dalgona-web/";

/// The one foreign connector this host is known to add: a recurring sweep
/// sending exactly this request to every 127.0.0.0/8 listener (captured
/// live: `GET / HTTP/1.1` with `User-Agent: Go-http-client/1.1`,
/// `Connection: close`, `Accept-Encoding: gzip`). Only this exact signature
/// is skipped; every other unexpected connection fails the test that sees
/// it, with its bounded request context.
const SCANNER_REQUEST_LINE: &str = "GET / HTTP/1.1";
const SCANNER_USER_AGENT: &str = "Go-http-client/1.1";

/// How long one connection may take to deliver its request head or body.
/// The fixture client writes at once, with a wide margin for a loaded host;
/// a connection that stalls this long is an unexpected one.
const STRANGER_WINDOW: Duration = Duration::from_secs(2);

/// The most bytes of one head the fixture reads before calling it
/// malformed, and the most bytes of context a failure message carries.
const HEAD_CAP: usize = 16 * 1024;
const CONTEXT_BYTES: usize = 512;

/// One parsed request head.
struct Head {
    request_line: String,
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    content_length: usize,
}

impl Head {
    /// The first header with a case-insensitive name match.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }

    /// Whether the request carries the battery's own user agent.
    fn is_from_the_fixture_client(&self) -> bool {
        self.header("User-Agent")
            .is_some_and(|agent| agent.starts_with(CLIENT_MARK))
    }

    /// Whether the request is exactly the known scanner sweep, byte for
    /// byte on its request line and user agent.
    fn is_the_known_scanner(&self) -> bool {
        self.request_line == SCANNER_REQUEST_LINE
            && self.header("User-Agent") == Some(SCANNER_USER_AGENT)
    }
}

/// Splits one raw request head into its request line and header fields.
fn parse_head(raw: &[u8]) -> Option<Head> {
    let head = String::from_utf8_lossy(raw);
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_owned();
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    Some(Head {
        request_line,
        method,
        target,
        headers,
        content_length,
    })
}

/// The bounded context of one connection: at most [`CONTEXT_BYTES`] of its
/// head, escaped for a failure message.
fn context(raw: &[u8]) -> String {
    let end = raw.len().min(CONTEXT_BYTES);
    String::from_utf8_lossy(&raw[..end])
        .escape_default()
        .to_string()
}

/// Reads one request head, ending at the blank line, within
/// [`STRANGER_WINDOW`] and [`HEAD_CAP`]. A connection that stalls, closes,
/// or outgrows the window yields `None`.
async fn read_head(stream: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + STRANGER_WINDOW;
    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => read,
            _ => return None,
        };
        raw.extend_from_slice(&chunk[..read]);
        if raw.len() > HEAD_CAP {
            return None;
        }
    }
    Some(raw)
}

/// Reads the body bytes beyond the head until `content_length` is met,
/// within [`STRANGER_WINDOW`] like [`read_head`].
async fn read_body_tail(
    stream: &mut tokio::net::TcpStream,
    content_length: usize,
    raw: &[u8],
) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let head_end = raw.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let mut request_body = raw[head_end..].to_vec();
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + STRANGER_WINDOW;
    while request_body.len() < content_length {
        let read = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(read)) if read > 0 => read,
            _ => return None,
        };
        request_body.extend_from_slice(&chunk[..read]);
    }
    Some(request_body)
}

/// Accepts connections until one carries a fixture-client request, records
/// that request, and answers `status` with `body` as `content_type`. Only
/// the known scanner sweep is skipped; a connection that sends nothing, a
/// malformed head, or any request that is neither the sweep nor the tool's
/// returns an error carrying its bounded context. The wait ends when
/// `client` settles unserved, which returns an error, never a hang.
pub(crate) async fn serve(
    listener: &tokio::net::TcpListener,
    status: u16,
    content_type: &str,
    body: &[u8],
    client: &mut tokio::task::JoinSet<Result<String, String>>,
) -> Result<Received, Box<dyn StdError>> {
    tokio::select! {
        served = next_client_request(listener, status, content_type, body) => match served {
            Ok(received) => Ok(received),
            Err(error) => {
                client.abort_all();
                while client.join_next().await.is_some() {}
                Err(error)
            }
        },
        settled = client.join_next() => match settled {
            Some(Ok(Ok(text))) => Err(std::io::Error::other(format!(
                "the search settled without its request reaching the listener: {text}"
            ))
            .into()),
            Some(Ok(Err(error))) => Err(std::io::Error::other(format!(
                "the search settled without its request reaching the listener: the tool failed: {error}"
            ))
            .into()),
            Some(Err(join)) => Err(std::io::Error::other(format!(
                "the search settled without its request reaching the listener: the tool task failed: {join}"
            ))
            .into()),
            None => Err(std::io::Error::other(
                "the search settled without its request reaching the listener: the tool task vanished",
            )
            .into()),
        },
    }
}

/// Serves one connection at a time until a fixture-client request appears.
/// A connection that is neither the known scanner sweep nor the tool's
/// request returns an error carrying its bounded context to the test that
/// owns the listener.
async fn next_client_request(
    listener: &tokio::net::TcpListener,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<Received, Box<dyn StdError>> {
    use tokio::io::AsyncWriteExt;
    loop {
        let (mut stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                return Err(std::io::Error::other(format!("accept a connection: {error}")).into());
            }
        };
        let Some(raw) = read_head(&mut stream).await else {
            return Err(std::io::Error::other(format!(
                "unexpected connection from {peer}: no head within the window"
            ))
            .into());
        };
        let Some(head) = parse_head(&raw) else {
            return Err(std::io::Error::other(format!(
                "unexpected connection from {peer}: {}",
                context(&raw)
            ))
            .into());
        };
        if head.is_the_known_scanner() {
            continue;
        }
        if !head.is_from_the_fixture_client() {
            return Err(std::io::Error::other(format!(
                "unexpected connection from {peer}: {}",
                context(&raw)
            ))
            .into());
        }
        let Some(request_body) = read_body_tail(&mut stream, head.content_length, &raw).await
        else {
            return Err(std::io::Error::other(format!(
                "unexpected connection from {peer}: body missing within the window"
            ))
            .into());
        };
        let reply = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        if let Err(error) = stream.write_all(reply.as_bytes()).await {
            return Err(std::io::Error::other(format!("write the head: {error}")).into());
        }
        if let Err(error) = stream.write_all(body).await {
            return Err(std::io::Error::other(format!("write the body: {error}")).into());
        }
        if let Err(error) = stream.shutdown().await {
            return Err(std::io::Error::other(format!("close the connection: {error}")).into());
        }
        return Ok(Received {
            method: head.method,
            target: head.target,
            headers: head.headers,
            body: request_body,
        });
    }
}

/// Watches `listener` for `window` and returns the bounded context of the
/// first connection that arrives, whatever it says, or `None` once the
/// window closes untouched. The denial listener binds `::1`, which no
/// ambient sweep visits, so any connection at all is an intrusion the test
/// must name.
pub(crate) async fn first_connection(
    listener: &tokio::net::TcpListener,
    window: Duration,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + window;
    let accepted = tokio::time::timeout_at(deadline, listener.accept()).await;
    let Ok(Ok((mut stream, peer))) = accepted else {
        return None;
    };
    let raw = read_head(&mut stream).await.unwrap_or_default();
    Some(format!("{peer} {}", context(&raw)))
}
