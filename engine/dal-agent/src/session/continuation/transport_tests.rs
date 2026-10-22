//! Loopback HTTP/2 proofs for the Cursor continuation slot.
//!
//! Each test drives production code: [`open_step`] and the [`ContinuationSlot`]
//! transitions (`park`, `park_session_calls`, `stash`, `respond`, `is_awaiting`)
//! over a real [`ProviderSet`]. Its admission semaphore is real, and so is its
//! HTTP/2 prior-knowledge client, installed through
//! `ProviderSet::set_test_client`. The peer is a hyper HTTP/2 server on
//! 127.0.0.1 that serves the Cursor `AgentService/Run` route.
//!
//! Assertions read wire observations (run count, HTTP version, bearer header,
//! uploaded bytes per run) and slot predicates. This file does not prove
//! private receipt or effect ordering: those need Cursor-family dispatch
//! through the synthetic loop, which this file does not drive.
//!
//! Frames are assembled from the Connect envelope and the field numbers in
//! `family::cursor::proto`. The real `RunStream` decoder is the oracle: a
//! wrong number or envelope changes the decoded events, and the assertions
//! on announced calls and stop reasons fail.

use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    ffi::OsString,
    future::poll_fn,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    pin::{Pin, pin},
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    task::{Context, Poll},
    time::Duration,
};

use dal_core::{
    CallId, Config, ConfigProduct, Family, ModelRequest, ModelRoute, ModelToolSpec, Part, Purpose,
    RawJson, RequestParams, SessionId, ThinkingLevel,
};
use dal_provider::{
    AuthStyle, CatalogEntry, EnvSnapshot, EventStream, HostToolResult, Listing, ProviderConfig,
    ProviderEntry, ProviderError, ProviderIdentity, ProviderSet, ProviderSettings, ResolvedModel,
    StopReason, StreamEvent, ThinkingSupport, ToolSupport, Transport,
    build_client_http2_prior_knowledge,
};
use hyper::{
    Method, Request, Response, StatusCode, Version,
    body::{Bytes, Frame, Incoming},
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue},
    server::conn::http2,
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Notify, mpsc},
    time::timeout,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::{ContinuationOwner, ContinuationSlot, ParkError, StashError, open_step};
use crate::{
    Env, Host, Product,
    host::HostState,
    session::{
        tasks::SessionTasks,
        turn::{RequestDeps, infer_stream},
    },
};

pub(super) const WAIT: Duration = Duration::from_secs(15);
const RUN_PATH: &str = "/agent.v1.AgentService/Run";
pub(super) const PROVIDER: &str = "cursor-h2";
pub(super) const MODEL: &str = "cursor-h2-model";
pub(super) const SECRET: &str = "cursor-h2-secret";

// ---------------------------------------------------------------------------
// Connect envelopes and protobuf fields (family::cursor::proto numbering).
// ---------------------------------------------------------------------------

/// Base-128 varint.
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let low = u8::try_from(value & 0x7f).expect("seven low bits fit a byte");
        value >>= 7;
        if value == 0 {
            out.push(low);
            return out;
        }
        out.push(low | 0x80);
    }
}

/// Length-delimited field `tag`.
fn field(tag: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = varint((tag << 3) | 2);
    out.extend(varint(
        u64::try_from(bytes.len()).expect("a length fits u64"),
    ));
    out.extend_from_slice(bytes);
    out
}

/// Varint field `tag`.
fn varint_field(tag: u64, value: u64) -> Vec<u8> {
    let mut out = varint(tag << 3);
    out.extend(varint(value));
    out
}

/// One Connect envelope: a zero flag byte, a big-endian length, the payload.
fn envelope(payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).expect("a payload fits the length prefix");
    let mut out = vec![0x00];
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A server frame announcing one tool call the client must answer.
/// `AgentServerMessage.exec_server_message` 2; `ExecServerMessage.id` 1 and
/// `mcp_args` 11; `McpArgs.name` 1, `tool_call_id` 3, `tool_name` 5.
pub(super) fn exec_frame(call: &str, tool: &str, exec_id: u64) -> Vec<u8> {
    let mut args = field(1, tool.as_bytes());
    args.extend(field(3, call.as_bytes()));
    args.extend(field(5, tool.as_bytes()));
    let mut exec = varint_field(1, exec_id);
    exec.extend(field(11, &args));
    envelope(&field(2, &exec))
}

/// A visible text delta. `AgentServerMessage.interaction_update` 1;
/// `InteractionUpdate.text_delta` 1; `TextDeltaUpdate.text` 1.
pub(super) fn text_frame(text: &str) -> Vec<u8> {
    let delta = field(1, text.as_bytes());
    let update = field(1, &delta);
    envelope(&field(1, &update))
}

/// The terminal update. `InteractionUpdate.turn_ended` 14;
/// `TurnEndedUpdate.input_tokens` 1, `output_tokens` 2.
pub(super) fn turn_ended_frame(input: u64, output: u64) -> Vec<u8> {
    let mut ended = varint_field(1, input);
    ended.extend(varint_field(2, output));
    let update = field(14, &ended);
    envelope(&field(1, &update))
}

/// The offset just past the first envelope of `upload`, once it is complete.
fn first_envelope_end(upload: &[u8]) -> Option<usize> {
    let [_, a, b, c, d, ..] = upload else {
        return None;
    };
    let len = usize::try_from(u32::from_be_bytes([*a, *b, *c, *d])).ok()?;
    let end = len.checked_add(5)?;
    (upload.len() >= end).then_some(end)
}

/// Counts the complete envelopes past `before`. Each complete envelope must
/// carry flag 0x00, and a trailing partial envelope is not judged yet. Returns
/// `None` if a complete envelope is malformed.
fn keepalive_count(upload: &[u8], before: usize) -> Option<usize> {
    let mut rest = upload.get(before..)?;
    let mut count = 0_usize;
    while let [flag, a, b, c, d, tail @ ..] = rest {
        if *flag != 0x00 {
            return None;
        }
        let len = usize::try_from(u32::from_be_bytes([*a, *b, *c, *d])).ok()?;
        let Some(next) = tail.get(len..) else {
            break;
        };
        count += 1;
        rest = next;
    }
    Some(count)
}

// ---------------------------------------------------------------------------
// Loopback HTTP/2 server: one script per Run request, in arrival order.
// ---------------------------------------------------------------------------

/// One directive for a single Run request.
pub(super) enum Step {
    /// Stream these response bytes.
    Send(Vec<u8>),
    /// Stream nothing further until the request upload contains `needle`.
    AwaitUpload(Vec<u8>),
    /// Keep the response open until the server stops.
    Hold,
}

/// Withholds a response head until `release`. `arrived` fires once the
/// request has reached the server and been recorded.
#[derive(Clone, Default)]
struct Gate {
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

/// The script for one Run request.
pub(super) struct RunScript {
    gate: Option<Gate>,
    steps: Vec<Step>,
}

impl RunScript {
    pub(super) fn new(steps: Vec<Step>) -> Self {
        Self { gate: None, steps }
    }

    fn gated(gate: Gate, steps: Vec<Step>) -> Self {
        Self {
            gate: Some(gate),
            steps,
        }
    }
}

/// What the server observed for one Run request.
#[derive(Default)]
struct RunRecord {
    upload: Vec<u8>,
    closed: bool,
}

/// One pending upload probe. `Wire::probes` owns it until it becomes due.
struct UploadProbe {
    /// The run whose request upload is watched.
    run: usize,
    /// The bytes whose completion in that upload makes the probe due.
    needle: Vec<u8>,
    /// Runs once on a blocking thread, before the completing chunk is observable.
    fire: Box<dyn FnOnce() + Send>,
}

/// Shared server state. `changed` wakes every condition wait on each update.
pub(super) struct Wire {
    scripts: Mutex<VecDeque<RunScript>>,
    runs: Mutex<Vec<RunRecord>>,
    versions: Mutex<Vec<Version>>,
    bearers: Mutex<Vec<String>>,
    changed: Notify,
    stop: CancellationToken,
    tracker: TaskTracker,
    probes: Mutex<Vec<UploadProbe>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Wire {
    pub(super) fn run_count(&self) -> usize {
        lock(&self.runs).len()
    }

    pub(super) fn upload(&self, run: usize) -> Vec<u8> {
        lock(&self.runs)
            .get(run)
            .map_or_else(Vec::new, |record| record.upload.clone())
    }

    pub(super) fn is_closed(&self, run: usize) -> bool {
        lock(&self.runs)
            .get(run)
            .is_some_and(|record| record.closed)
    }

    fn occurrences(&self, run: usize, needle: &[u8]) -> usize {
        if needle.is_empty() {
            return 0;
        }
        lock(&self.runs).get(run).map_or(0, |record| {
            record
                .upload
                .windows(needle.len())
                .filter(|window| *window == needle)
                .count()
        })
    }

    pub(super) fn h2_only(&self) -> bool {
        let versions = lock(&self.versions);
        !versions.is_empty() && versions.iter().all(|version| *version == Version::HTTP_2)
    }

    pub(super) fn bearer_seen(&self, token: &str) -> bool {
        let expected = format!("Bearer {token}");
        lock(&self.bearers).contains(&expected)
    }

    /// Registers `fire` to run once, before the chunk that first completes
    /// `needle` in run `run`'s upload becomes observable to the peer.
    pub(super) fn probe_upload(&self, run: usize, needle: Vec<u8>, fire: Box<dyn FnOnce() + Send>) {
        lock(&self.probes).push(UploadProbe { run, needle, fire });
    }

    /// Appends one received chunk to run `run`. Probes that the chunk completes
    /// run on a blocking thread and finish before the chunk is visible, so a
    /// peer that waits for the chunk answers only after its probes have run.
    async fn observe_upload(&self, run: usize, chunk: &[u8]) {
        for fire in self.take_due_probes(run, chunk) {
            tokio::task::spawn_blocking(fire)
                .await
                .expect("the upload probe runs to completion");
        }
        lock(&self.runs)[run].upload.extend_from_slice(chunk);
    }

    /// Removes and returns the probes that `chunk` completes for run `run`.
    fn take_due_probes(&self, run: usize, chunk: &[u8]) -> Vec<Box<dyn FnOnce() + Send>> {
        let runs = lock(&self.runs);
        let mut probes = lock(&self.probes);
        if !probes.iter().any(|probe| probe.run == run) {
            return Vec::new();
        }
        let mut completed = runs[run].upload.clone();
        completed.extend_from_slice(chunk);
        let (due, rest): (Vec<UploadProbe>, Vec<UploadProbe>) = probes
            .drain(..)
            .partition(|probe| probe.run == run && contains(&completed, &probe.needle));
        *probes = rest;
        due.into_iter().map(|probe| probe.fire).collect()
    }
}

/// Whether `needle` occurs in `haystack`. An empty needle never occurs.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Waits until `condition` holds. Each check registers for the next wake
/// before testing, so no update is missed.
pub(super) async fn wait_until(wire: &Wire, what: &str, condition: impl Fn() -> bool) {
    let waited = timeout(WAIT, async {
        loop {
            let changed = wire.changed.notified();
            if condition() {
                return;
            }
            changed.await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting for {what}");
}

/// Waits for run `run`'s request body to close, then counts `needle` in it.
pub(super) async fn uploaded_once_settled(wire: &Wire, run: usize, needle: &str) -> usize {
    wait_until(wire, "the request body to close", || wire.is_closed(run)).await;
    wire.occurrences(run, needle.as_bytes())
}

struct ChunkBody {
    rx: mpsc::Receiver<Bytes>,
}

impl hyper::body::Body for ChunkBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.get_mut()
            .rx
            .poll_recv(cx)
            .map(|item| item.map(|bytes| Ok(Frame::data(bytes))))
    }
}

fn status(code: StatusCode) -> Response<ChunkBody> {
    let (_closed, rx) = mpsc::channel::<Bytes>(1);
    let mut response = Response::new(ChunkBody { rx });
    *response.status_mut() = code;
    response
}

/// Records the upload of run `run` until its request body ends.
async fn drain_upload(wire: Arc<Wire>, run: usize, body: Incoming, stop: CancellationToken) {
    use hyper::body::Body as _;
    let mut body = pin!(body);
    loop {
        let next = tokio::select! {
            biased;
            () = stop.cancelled() => return,
            frame = poll_fn(|cx| body.as_mut().poll_frame(cx)) => frame,
        };
        let Some(Ok(frame)) = next else {
            break;
        };
        if let Ok(data) = frame.into_data() {
            wire.observe_upload(run, &data).await;
            wire.changed.notify_waiters();
        }
    }
    lock(&wire.runs)[run].closed = true;
    wire.changed.notify_waiters();
}

/// Waits until run `run`'s upload contains `needle`. False on timeout or cancellation.
async fn await_upload(wire: &Wire, run: usize, needle: &[u8], stop: &CancellationToken) -> bool {
    let waited = timeout(WAIT, async {
        loop {
            let changed = wire.changed.notified();
            if wire.occurrences(run, needle) > 0 {
                return true;
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => return false,
                () = changed => {}
            }
        }
    })
    .await;
    waited.unwrap_or(false)
}

/// Plays the scripted response of run `run`. Dropping `tx` ends the body.
async fn run_steps(
    wire: Arc<Wire>,
    run: usize,
    steps: Vec<Step>,
    tx: mpsc::Sender<Bytes>,
    stop: CancellationToken,
) {
    for step in steps {
        let proceed = match step {
            Step::Send(bytes) => {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => false,
                    res = tx.send(Bytes::from(bytes)) => res.is_ok(),
                }
            }
            Step::AwaitUpload(needle) => await_upload(&wire, run, &needle, &stop).await,
            Step::Hold => {
                stop.cancelled().await;
                false
            }
        };
        if !proceed {
            return;
        }
    }
}

async fn serve(
    wire: Arc<Wire>,
    request: Request<Incoming>,
    stop: CancellationToken,
) -> Response<ChunkBody> {
    if stop.is_cancelled() {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    lock(&wire.versions).push(request.version());
    if let Some(value) = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        lock(&wire.bearers).push(value.to_owned());
    }
    wire.changed.notify_waiters();
    if request.method() != Method::POST || request.uri().path() != RUN_PATH {
        return status(StatusCode::NOT_FOUND);
    }
    let script = lock(&wire.scripts)
        .pop_front()
        .unwrap_or_else(|| RunScript::new(Vec::new()));
    let run = {
        let mut runs = lock(&wire.runs);
        runs.push(RunRecord::default());
        runs.len() - 1
    };
    wire.changed.notify_waiters();
    if let Some(gate) = &script.gate {
        gate.arrived.notify_one();
        tokio::select! {
            biased;
            () = gate.release.notified() => {}
            () = stop.cancelled() => return status(StatusCode::SERVICE_UNAVAILABLE),
        }
    }
    let body = request.into_body();
    wire.tracker
        .spawn(drain_upload(Arc::clone(&wire), run, body, stop.clone()));
    let (tx, rx) = mpsc::channel::<Bytes>(64);
    wire.tracker
        .spawn(run_steps(Arc::clone(&wire), run, script.steps, tx, stop));
    let mut response = Response::new(ChunkBody { rx });
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/connect+proto"),
    );
    response
}

/// Accepts connections until the server stops, serving each one over HTTP/2.
async fn accept_loop(listener: TcpListener, wire: Arc<Wire>) {
    loop {
        let accepted = tokio::select! {
            biased;
            () = wire.stop.cancelled() => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, _)) = accepted else {
            return;
        };
        wire.tracker
            .spawn(serve_connection(stream, Arc::clone(&wire)));
    }
}

/// Serves one accepted connection with HTTP/2 prior knowledge.
async fn serve_connection(stream: TcpStream, wire: Arc<Wire>) {
    let conn_stop = wire.stop.child_token();
    let service = service_fn({
        let wire = Arc::clone(&wire);
        let conn_stop = conn_stop.clone();
        move |request: Request<Incoming>| {
            let wire = Arc::clone(&wire);
            let conn_stop = conn_stop.clone();
            async move { Ok::<_, Infallible>(serve(wire, request, conn_stop).await) }
        }
    });
    let conn =
        http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(stream), service);
    let mut conn = pin!(conn);
    tokio::select! {
        biased;
        () = wire.stop.cancelled() => {
            conn.as_mut().graceful_shutdown();
            let _ = timeout(WAIT, conn).await;
        }
        _ = conn.as_mut() => {}
    }
    conn_stop.cancel();
}

pub(super) struct CursorH2 {
    addr: SocketAddr,
    pub(super) wire: Arc<Wire>,
}

impl CursorH2 {
    pub(super) async fn start(scripts: Vec<RunScript>) -> Self {
        let wire = Arc::new(Wire {
            scripts: Mutex::new(scripts.into()),
            runs: Mutex::new(Vec::new()),
            versions: Mutex::new(Vec::new()),
            bearers: Mutex::new(Vec::new()),
            changed: Notify::new(),
            stop: CancellationToken::new(),
            tracker: TaskTracker::new(),
            probes: Mutex::new(Vec::new()),
        });
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("the loopback listener binds");
        let addr = listener.local_addr().expect("the loopback address");
        wire.tracker.spawn(accept_loop(listener, Arc::clone(&wire)));
        Self { addr, wire }
    }

    pub(super) fn base_url(&self) -> String {
        format!("http://{}:{}/", self.addr.ip(), self.addr.port())
    }

    pub(super) async fn shutdown(&self) {
        self.wire.stop.cancel();
        self.wire.changed.notify_waiters();
        self.wire.tracker.close();
        let waited = timeout(WAIT, self.wire.tracker.wait()).await;
        assert!(
            waited.is_ok(),
            "timed out waiting for server tasks to shut down"
        );
    }
}

impl Drop for CursorH2 {
    fn drop(&mut self) {
        self.wire.stop.cancel();
    }
}

// ---------------------------------------------------------------------------
// Real provider set, provider handle, request, host, and step helpers.
// ---------------------------------------------------------------------------

/// One Cursor provider on `base_url`, with the HTTP/2 prior-knowledge client
/// installed before any provider handle exists.
fn cursor_set(max_concurrent: u32, base_url: &str, dir: &Path) -> ProviderSet {
    let entry = ProviderEntry {
        id: PROVIDER.into(),
        def: None,
        family: Family::Cursor,
        base_url: base_url.into(),
        transport: Transport::Https,
        key_env: Some("CURSOR_H2_KEY".into()),
        auth: AuthStyle::Bearer,
        max_concurrent_requests: max_concurrent,
        settings: ProviderSettings::default(),
    };
    let config = ProviderConfig {
        default_model: None,
        thinking: ThinkingLevel::Medium,
        aliases: Vec::new(),
        request_max_retries: 0,
        stream_max_retries: 0,
        providers: vec![entry],
        scripted: None,
    };
    let identity = ProviderIdentity {
        version: "test".into(),
        os: "linux".into(),
        os_version: "test".into(),
        arch: "x86_64".into(),
    };
    let env = EnvSnapshot::test(&[("CURSOR_H2_KEY", SECRET)]);
    let mut set =
        ProviderSet::new(&config, identity, env, dir, dir).expect("the provider set builds");
    set.set_test_client(PROVIDER, build_client_http2_prior_knowledge())
        .expect("the loopback HTTP/2 client installs");
    set
}

/// The Cursor provider handle for [`MODEL`].
fn cursor_provider(set: &ProviderSet) -> dal_provider::Provider {
    let entry = CatalogEntry {
        provider: PROVIDER.into(),
        id: MODEL.into(),
        display: MODEL.into(),
        listing: Listing::Listed,
        context_window: Some(32_000),
        max_output: Some(4_096),
        thinking: ThinkingSupport::OpenAi {
            accepted: vec![ThinkingLevel::Low, ThinkingLevel::Medium],
            none_supported: true,
        },
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: true,
        display_supported: false,
        premium_multiplier: None,
        price: None,
    };
    let resolved = ResolvedModel {
        provider: PROVIDER.into(),
        route: ModelRoute::Api {
            family: Family::Cursor,
            model: MODEL.into(),
        },
        entry,
    };
    set.provider(resolved).expect("the Cursor provider binds")
}

/// A request advertising `tools` under their own names.
fn cursor_request(tools: &[&str]) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Api {
            family: Family::Cursor,
            model: MODEL.into(),
        },
        system: "You are a test assistant.".into(),
        tools: tools
            .iter()
            .map(|name| ModelToolSpec {
                name: (*name).into(),
                description: "Test tool.".into(),
                parameters: RawJson::parse(r#"{"type":"object"}"#).expect("schema json"),
                grammar: None,
                defer_loading: false,
            })
            .collect::<Vec<_>>()
            .into(),
        context: Vec::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn result(call: &str, name: &str, text: &str) -> HostToolResult {
    HostToolResult {
        call: CallId::new(call),
        name: name.into(),
        is_error: false,
        parts: vec![Part::Text { text: text.into() }],
    }
}

/// A distinct payload per result, so wire counts identify each delivery.
pub(super) fn marker(tag: &str) -> String {
    format!("RESULT-MARK-{tag}")
}

type Events = Vec<Result<StreamEvent, ProviderError>>;

/// Drains one step to its terminal event or its error.
async fn drain(mut stream: EventStream) -> Events {
    let mut events = Vec::new();
    while let Some(item) = timeout(WAIT, stream.next())
        .await
        .expect("the step makes progress")
    {
        let finished = matches!(item, Err(_) | Ok(StreamEvent::Stop { .. }));
        events.push(item);
        if finished {
            break;
        }
    }
    events
}

/// The (id, name) pairs a step announced, in order.
fn announced(events: &Events) -> Vec<(CallId, Box<str>)> {
    let mut calls = Vec::new();
    for event in events {
        if let Ok(StreamEvent::ToolCallsDone { calls: done }) = event {
            calls.extend(done.iter().map(|call| {
                (
                    CallId::new(call.id.as_str()),
                    Box::<str>::from(call.name.as_str()),
                )
            }));
        }
    }
    calls
}

fn ends_in_tool_use(events: &Events) -> bool {
    matches!(
        events.last(),
        Some(Ok(StreamEvent::Stop { reason })) if matches!(reason, StopReason::ToolUse)
    )
}

fn ends_in_turn(events: &Events) -> bool {
    matches!(
        events.last(),
        Some(Ok(StreamEvent::Stop { reason })) if !matches!(reason, StopReason::ToolUse)
    )
}

/// A real host, so the request carries a real `HostState`.
async fn host_state(dir: &Path) -> (Arc<HostState>, Host) {
    let data = dir.join("data");
    let workspace_dir = dir.join("w");
    std::fs::create_dir_all(&data).expect("the data dir exists");
    std::fs::create_dir_all(&workspace_dir).expect("the workspace dir exists");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, "").expect("the fixture writes");
    let user = format!(
        "model = \"acme/exploder\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))
        .expect("the config loads");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: std::collections::BTreeMap::new(),
        cwd: workspace_dir,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("the host starts");
    (Arc::clone(&host.state), host)
}

fn slot_of(owner: &ContinuationOwner) -> Arc<ContinuationSlot> {
    owner.downgrade().upgrade().expect("the owner is alive")
}

/// Opens one step for `owner`'s slot and returns its stream.
async fn open(
    state: &Arc<HostState>,
    session: SessionId,
    owner: &ContinuationOwner,
    provider: dal_provider::Provider,
    request: &ModelRequest,
    cancel: &CancellationToken,
) -> EventStream {
    let deps = RequestDeps {
        session,
        host: Arc::clone(state),
        script: None,
        continuation: Some(owner.downgrade()),
    };
    timeout(WAIT, open_step(&deps, provider, request, cancel))
        .await
        .expect("the open makes progress")
        .expect("the open succeeds")
}

// ---------------------------------------------------------------------------
// Proofs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consecutive_tool_rounds_with_fresh_ids_share_one_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame("call-a1", "session_tool", 1)),
        Step::AwaitUpload(marker("A").into_bytes()),
        Step::Send(exec_frame("call-b1", "session_tool", 2)),
        Step::AwaitUpload(marker("B").into_bytes()),
        Step::Send(text_frame("done")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(4, &server.base_url(), dir.path());
    let session = SessionId::new_v7();
    let owner = ContinuationOwner::new(SessionTasks::new());
    let slot = slot_of(&owner);
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let first = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&first), "round one parks: {first:?}");
    let pending = announced(&first);
    assert_eq!(pending.len(), 1, "round one announces one call");
    assert_eq!(pending[0].0, CallId::new("call-a1"));
    slot.park(pending).expect("round one parks");
    slot.respond(
        vec![result("call-a1", "session_tool", &marker("A"))],
        &cancel,
    )
    .await
    .expect("round one resumes");

    let second = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&second), "round two parks: {second:?}");
    let pending = announced(&second);
    assert_eq!(pending[0].0, CallId::new("call-b1"));
    slot.park(pending)
        .expect("a fresh id parks once the prior cover is cleared");
    slot.respond(
        vec![result("call-b1", "session_tool", &marker("B"))],
        &cancel,
    )
    .await
    .expect("round two resumes");

    let last = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_turn(&last), "the turn ends: {last:?}");
    assert!(!slot.is_awaiting());
    assert!(owner.release_if_live());

    assert_eq!(
        server.wire.run_count(),
        1,
        "three rounds share one Run request"
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("A")).await,
        1
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("B")).await,
        1
    );
    assert!(server.wire.h2_only(), "every request spoke HTTP/2");
    assert!(
        server.wire.bearer_seen(SECRET),
        "the configured credential reached the wire"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn mixed_round_parks_the_full_cover_then_the_session_subset_and_delivers_each_result_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame("call-p1", "private_tool", 1)),
        Step::Send(exec_frame("call-s1", "session_tool", 2)),
        Step::AwaitUpload(marker("S").into_bytes()),
        Step::Send(text_frame("done")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(4, &server.base_url(), dir.path());
    let session = SessionId::new_v7();
    let owner = ContinuationOwner::new(SessionTasks::new());
    let slot = slot_of(&owner);
    let cancel = CancellationToken::new();
    let request = cursor_request(&["private_tool", "session_tool"]);

    let first = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&first), "the mixed step parks: {first:?}");
    let pending = announced(&first);
    assert_eq!(
        pending.len(),
        2,
        "one private and one session call are announced"
    );
    slot.park(pending).expect("the full cover parks");

    let wrong = slot.park_session_calls(vec![(CallId::new("call-s1"), "other_tool".into())]);
    assert!(
        matches!(wrong, Err(ParkError::WrongName { .. })),
        "a re-parked session call must name its parked tool"
    );
    slot.park_session_calls(vec![(CallId::new("call-s1"), "session_tool".into())])
        .expect("the session subset re-parks without replacing the full cover");

    slot.stash(
        CallId::new("call-p1"),
        "private_tool".into(),
        false,
        vec![Part::Text {
            text: marker("P").into(),
        }],
    )
    .expect("the private result is stashed");
    slot.respond(
        vec![result("call-s1", "session_tool", &marker("S"))],
        &cancel,
    )
    .await
    .expect("the cover resumes");

    let last = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_turn(&last), "the turn ends: {last:?}");
    assert!(owner.release_if_live());

    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("P")).await,
        1,
        "the stashed result is delivered once"
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("S")).await,
        1,
        "the session result is delivered once"
    );
    assert_eq!(server.wire.run_count(), 1);
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn cancelled_resume_under_blocked_admission_keeps_the_stash_and_retries_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![
        RunScript::new(vec![
            Step::Send(exec_frame("call-a1", "session_tool", 1)),
            Step::AwaitUpload(marker("A").into_bytes()),
            Step::Send(text_frame("done")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
        RunScript::new(vec![Step::Hold]),
    ])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(1, &server.base_url(), dir.path());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let a_session = SessionId::new_v7();
    let a = ContinuationOwner::new(SessionTasks::new());
    let a_slot = slot_of(&a);
    let first = drain(
        open(
            &state,
            a_session,
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&first));
    a_slot.park(announced(&first)).expect("the step parks");
    a_slot
        .stash(
            CallId::new("call-a1"),
            "session_tool".into(),
            false,
            vec![Part::Text {
                text: marker("A").into(),
            }],
        )
        .expect("the private result is stashed");

    // A second session takes the only admission permit and keeps its stream.
    let b = ContinuationOwner::new(SessionTasks::new());
    let held = open(
        &state,
        SessionId::new_v7(),
        &b,
        cursor_provider(&set),
        &request,
        &cancel,
    )
    .await;

    let resume_cancel = CancellationToken::new();
    {
        let mut resume = pin!(a_slot.respond(Vec::new(), &resume_cancel));
        assert!(
            futures::poll!(resume.as_mut()).is_pending(),
            "the resume waits while the permit is held"
        );
        resume_cancel.cancel();
        assert!(
            resume.await.is_err(),
            "a cancelled admission fails the resume"
        );
    }
    assert!(
        a_slot.is_awaiting(),
        "a cancelled resume leaves the run awaiting"
    );

    drop(held);
    a_slot
        .respond(Vec::new(), &cancel)
        .await
        .expect("the retry is admitted and delivers the stashed result");
    let rest = drain(
        open(
            &state,
            a_session,
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_turn(&rest), "the turn ends: {rest:?}");
    assert!(a.release_if_live());

    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("A")).await,
        1,
        "the stashed result reaches the wire exactly once"
    );
    assert_eq!(server.wire.run_count(), 2, "the resume reused run A");
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn dropped_resume_under_blocked_admission_keeps_the_stash_and_retries_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![
        RunScript::new(vec![
            Step::Send(exec_frame("call-a1", "session_tool", 1)),
            Step::AwaitUpload(marker("A").into_bytes()),
            Step::Send(text_frame("done")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
        RunScript::new(vec![Step::Hold]),
    ])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(1, &server.base_url(), dir.path());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let a_session = SessionId::new_v7();
    let a = ContinuationOwner::new(SessionTasks::new());
    let a_slot = slot_of(&a);
    let first = drain(
        open(
            &state,
            a_session,
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&first));
    a_slot.park(announced(&first)).expect("the step parks");
    a_slot
        .stash(
            CallId::new("call-a1"),
            "session_tool".into(),
            false,
            vec![Part::Text {
                text: marker("A").into(),
            }],
        )
        .expect("the private result is stashed");

    let b = ContinuationOwner::new(SessionTasks::new());
    let held = open(
        &state,
        SessionId::new_v7(),
        &b,
        cursor_provider(&set),
        &request,
        &cancel,
    )
    .await;

    let resume_cancel = CancellationToken::new();
    {
        let mut resume = pin!(a_slot.respond(Vec::new(), &resume_cancel));
        assert!(
            futures::poll!(resume.as_mut()).is_pending(),
            "the resume waits while the permit is held"
        );
    }
    assert!(
        a_slot.is_awaiting(),
        "a dropped resume leaves the run awaiting"
    );

    drop(held);
    a_slot
        .respond(Vec::new(), &cancel)
        .await
        .expect("the retry is admitted and delivers the stashed result");
    let rest = drain(
        open(
            &state,
            a_session,
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_turn(&rest), "the turn ends: {rest:?}");
    assert!(a.release_if_live());

    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("A")).await,
        1,
        "the stashed result reaches the wire exactly once"
    );
    assert_eq!(server.wire.run_count(), 2, "the resume reused run A");
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn a_parked_run_holds_no_permit_and_permits_return_at_terminal_and_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![
        RunScript::new(vec![
            Step::Send(exec_frame("call-a1", "session_tool", 1)),
            Step::Hold,
        ]),
        RunScript::new(vec![
            Step::Send(text_frame("b")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
        RunScript::new(vec![Step::Hold]),
        RunScript::new(vec![
            Step::Send(text_frame("d")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
    ])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(1, &server.base_url(), dir.path());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let a = ContinuationOwner::new(SessionTasks::new());
    let a_slot = slot_of(&a);
    let parked = drain(
        open(
            &state,
            SessionId::new_v7(),
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&parked));
    a_slot
        .park(announced(&parked))
        .expect("the first run parks");

    let b = ContinuationOwner::new(SessionTasks::new());
    let b_events = drain(
        open(
            &state,
            SessionId::new_v7(),
            &b,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(
        ends_in_turn(&b_events),
        "a second session completes while the first is parked"
    );
    assert!(b.release_if_live());

    let c = ContinuationOwner::new(SessionTasks::new());
    let streaming = open(
        &state,
        SessionId::new_v7(),
        &c,
        cursor_provider(&set),
        &request,
        &cancel,
    )
    .await;
    drop(streaming);

    let d = ContinuationOwner::new(SessionTasks::new());
    let d_events = drain(
        open(
            &state,
            SessionId::new_v7(),
            &d,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(
        ends_in_turn(&d_events),
        "a dropped stream returns its permit"
    );
    assert!(d.release_if_live());

    assert!(
        a_slot.is_awaiting(),
        "the parked run still awaits its result"
    );
    assert!(a.release_if_live());
    assert_eq!(server.wire.run_count(), 4);
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn an_awaiting_run_keeps_its_heartbeat_and_holds_no_permit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![
        RunScript::new(vec![
            Step::Send(exec_frame("call-a1", "session_tool", 1)),
            Step::Hold,
        ]),
        RunScript::new(vec![
            Step::Send(text_frame("b")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
    ])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(1, &server.base_url(), dir.path());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let a = ContinuationOwner::new(SessionTasks::new());
    let a_slot = slot_of(&a);
    let parked = drain(
        open(
            &state,
            SessionId::new_v7(),
            &a,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&parked));
    a_slot.park(announced(&parked)).expect("the run parks");

    wait_until(
        &server.wire,
        "the run request envelope is on the parked body",
        || first_envelope_end(&server.wire.upload(0)).is_some(),
    )
    .await;
    let before = first_envelope_end(&server.wire.upload(0))
        .expect("the wait proved a complete run request envelope");
    wait_until(
        &server.wire,
        "a keepalive envelope past the run request",
        || keepalive_count(&server.wire.upload(0), before).is_some_and(|count| count >= 1),
    )
    .await;
    assert!(
        a_slot.is_awaiting(),
        "keepalive traffic does not resolve the parked run"
    );

    let b = ContinuationOwner::new(SessionTasks::new());
    let b_events = drain(
        open(
            &state,
            SessionId::new_v7(),
            &b,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(
        ends_in_turn(&b_events),
        "a second session completes while the first keeps alive"
    );
    assert!(
        a_slot.is_awaiting(),
        "the first run still awaits after the second completes"
    );
    assert!(a.release_if_live());
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn concurrent_opens_send_exactly_one_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(text_frame("done")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(4, &server.base_url(), dir.path());
    let owner = ContinuationOwner::new(SessionTasks::new());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);
    let deps = RequestDeps {
        session: SessionId::new_v7(),
        host: Arc::clone(&state),
        script: None,
        continuation: Some(owner.downgrade()),
    };

    let first = open_step(&deps, cursor_provider(&set), &request, &cancel);
    let second = open_step(&deps, cursor_provider(&set), &request, &cancel);
    let (a, b) = tokio::join!(first, second);
    let refused: Vec<String> = [&a, &b]
        .into_iter()
        .filter_map(|outcome| outcome.as_ref().err())
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "exactly one concurrent open is refused: {refused:?}"
    );
    assert!(
        refused[0].contains("no ready step"),
        "the loser is refused before any wire traffic: {}",
        refused[0]
    );
    assert!(a.is_ok() || b.is_ok(), "one open wins");
    assert_eq!(server.wire.run_count(), 1, "one handshake reached the wire");
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn dropped_open_and_released_slot_never_strand_or_send() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gate = Gate::default();
    let server = CursorH2::start(vec![
        RunScript::gated(gate.clone(), vec![Step::Hold]),
        RunScript::new(vec![
            Step::Send(text_frame("done")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
    ])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(4, &server.base_url(), dir.path());
    let session = SessionId::new_v7();
    let owner = ContinuationOwner::new(SessionTasks::new());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);
    let deps = RequestDeps {
        session,
        host: Arc::clone(&state),
        script: None,
        continuation: Some(owner.downgrade()),
    };

    {
        let mut opening = pin!(open_step(&deps, cursor_provider(&set), &request, &cancel));
        tokio::select! {
            biased;
            () = gate.arrived.notified() => {}
            outcome = &mut opening => {
                panic!("the response head arrived before the gate: {:?}", outcome.err());
            }
        }
    }

    let events = drain(
        open(
            &state,
            session,
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(
        ends_in_turn(&events),
        "the slot is not stranded: {events:?}"
    );
    assert_eq!(
        server.wire.run_count(),
        2,
        "the dropped attempt reached the server and the retry opened its own run"
    );

    assert!(owner.release_if_live());
    let refused = timeout(
        WAIT,
        open_step(&deps, cursor_provider(&set), &request, &cancel),
    )
    .await
    .expect("a released slot answers promptly");
    assert!(refused.is_err(), "a released slot refuses new steps");
    assert_eq!(server.wire.run_count(), 2, "a refused open sends nothing");
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn stray_stash_and_missing_cover_never_reach_the_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame("call-p1", "private_tool", 1)),
        Step::Hold,
    ])])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(4, &server.base_url(), dir.path());
    let owner = ContinuationOwner::new(SessionTasks::new());
    let slot = slot_of(&owner);
    let cancel = CancellationToken::new();
    let request = cursor_request(&["private_tool"]);

    let first = drain(
        open(
            &state,
            SessionId::new_v7(),
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&first));
    slot.park(announced(&first)).expect("the step parks");

    let stray = slot.stash(
        CallId::new("call-zzz"),
        "private_tool".into(),
        false,
        vec![Part::Text {
            text: marker("STRAY").into(),
        }],
    );
    assert!(
        matches!(stray, Err(StashError::UnknownCall(_))),
        "a result for an unparked call is refused"
    );
    let wrong_name = slot.stash(
        CallId::new("call-p1"),
        "other_tool".into(),
        false,
        Vec::new(),
    );
    assert!(
        matches!(wrong_name, Err(StashError::WrongName { .. })),
        "a result under the wrong tool name is refused"
    );
    assert!(
        slot.respond(Vec::new(), &cancel).await.is_err(),
        "a missing cover is refused, never filled"
    );
    assert!(
        slot.is_awaiting(),
        "a refused resume leaves the run awaiting"
    );

    assert!(owner.release_if_live());
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("STRAY")).await,
        0,
        "the stray result never reaches the wire"
    );
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

#[tokio::test]
async fn dropping_an_owner_without_release_ends_its_parked_run_on_the_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame("call-a1", "session_tool", 1)),
        Step::Hold,
    ])])
    .await;
    let (state, _host) = host_state(dir.path()).await;
    let set = cursor_set(1, &server.base_url(), dir.path());
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let owner = ContinuationOwner::new(SessionTasks::new());
    let slot = slot_of(&owner);
    let parked = drain(
        open(
            &state,
            SessionId::new_v7(),
            &owner,
            cursor_provider(&set),
            &request,
            &cancel,
        )
        .await,
    )
    .await;
    assert!(ends_in_tool_use(&parked));
    slot.park(announced(&parked)).expect("the run parks");
    drop(slot);
    drop(owner);

    wait_until(
        &server.wire,
        "the dropped owner ends its parked run on the wire",
        || server.wire.is_closed(0),
    )
    .await;
    assert!(server.wire.h2_only());
    server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Invocation-root fanout proofs: slot-less vs owner-carrying inference.
// ---------------------------------------------------------------------------

/// A real host whose Cursor provider points at the loopback peer, so one
/// inference enters through [`infer_stream`]: catalog resolve, provider
/// bind, and the Cursor branch. No extension is registered: the request
/// already names the Cursor route, so no synthetic route intervenes.
fn fanout_host(dir: &Path, base_url: &str) -> (Arc<HostState>, Host) {
    let data = dir.join("data");
    let workspace_dir = dir.join("w");
    std::fs::create_dir_all(&data).expect("the data dir exists");
    std::fs::create_dir_all(&workspace_dir).expect("the workspace dir exists");
    let user = format!(
        "model = \"{PROVIDER}/{MODEL}\"\n\n[providers.{PROVIDER}]\napi = \"cursor\"\nbase_url = \"{base_url}\"\nkey_env = \"CURSOR_H2_KEY\"\nauth = \"bearer\"\n\n[aliases]\n{MODEL} = \"{PROVIDER}/{MODEL}\"\n"
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str()))
        .expect("the config loads");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: Vec::new(),
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("CURSOR_H2_KEY"), OsString::from(SECRET))]),
        cwd: workspace_dir,
        sandbox_helper: None,
    };
    let host = Host::start_with_provider_setup(product, config, env, |set| {
        set.set_test_client(PROVIDER, build_client_http2_prior_knowledge())
            .expect("the loopback HTTP/2 client installs before the set is shared");
        Ok(())
    })
    .expect("the host starts with the loopback client");
    (Arc::clone(&host.state), host)
}

/// Drains one inference through [`infer_stream`] with `continuation` as its
/// slot handle: `None` is the `hook_fanout` shape (`hook_caller` mints a
/// caller with no attached slot), `Some` is the `hook_caller_with_continuation`
/// shape (the invocation root's downgrade attached for the same invocation).
async fn fanout_step(
    state: &Arc<HostState>,
    session: SessionId,
    continuation: Option<Weak<ContinuationSlot>>,
    request: &ModelRequest,
    cancel: &CancellationToken,
) -> Events {
    let deps = RequestDeps {
        session,
        host: Arc::clone(state),
        script: None,
        continuation,
    };
    drain(infer_stream(&deps, request.clone(), cancel).await).await
}

/// A slot-less fanout invocation opens a fresh provider run every time: the
/// first step's conversation ends with its stream, so the second invocation
/// handshakes again instead of resuming. Two invocations, two runs.
#[tokio::test]
async fn slotless_fanout_inference_opens_a_fresh_run_per_invocation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![
        RunScript::new(vec![
            Step::Send(exec_frame("call-a1", "session_tool", 1)),
            Step::Hold,
        ]),
        // A fresh text-only turn: the second invocation handshakes a new
        // run because the first step's conversation ended with its stream.
        RunScript::new(vec![
            Step::Send(text_frame("done")),
            Step::Send(turn_ended_frame(1, 1)),
        ]),
    ])
    .await;
    let (state, _host) = fanout_host(dir.path(), &server.base_url());
    let session = SessionId::new_v7();
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let first = fanout_step(&state, session, None, &request, &cancel).await;
    assert!(
        ends_in_tool_use(&first),
        "the first fanout invocation parks its step: {first:?}"
    );
    assert_eq!(
        announced(&first),
        vec![(CallId::new("call-a1"), "session_tool".into())],
        "the first step announces its call"
    );
    // No slot, so nothing parks: the conversation ends with this stream.
    let last = fanout_step(&state, session, None, &request, &cancel).await;
    assert!(
        ends_in_turn(&last),
        "the second fanout invocation ends: {last:?}"
    );
    assert_eq!(
        server.wire.run_count(),
        2,
        "slot-less fanout inference opens a fresh run per invocation; no durable run survives"
    );
    assert!(server.wire.h2_only(), "every request spoke HTTP/2");
    server.shutdown().await;
}

/// An owner-carrying fanout invocation reuses one durable run across steps:
/// the first step parks the mixed private-plus-session cover, the private
/// result stashes, the session cover responds, and the next invocation
/// resumes the same run. One run carries both results exactly once.
#[tokio::test]
async fn owner_carrying_fanout_inference_reuses_one_run_for_the_mixed_cover() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame("call-p1", "private_tool", 1)),
        Step::Send(exec_frame("call-s1", "session_tool", 2)),
        Step::AwaitUpload(marker("S").into_bytes()),
        Step::Send(text_frame("done")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let (state, _host) = fanout_host(dir.path(), &server.base_url());
    let session = SessionId::new_v7();
    let owner = ContinuationOwner::new(SessionTasks::new());
    let slot = slot_of(&owner);
    let cancel = CancellationToken::new();
    let request = cursor_request(&["private_tool", "session_tool"]);

    let first = fanout_step(&state, session, Some(owner.downgrade()), &request, &cancel).await;
    assert!(ends_in_tool_use(&first), "the mixed step parks: {first:?}");
    let pending = announced(&first);
    assert_eq!(
        pending.len(),
        2,
        "one private and one session call are announced"
    );
    slot.park(pending).expect("the full cover parks");
    slot.stash(
        CallId::new("call-p1"),
        "private_tool".into(),
        false,
        vec![Part::Text {
            text: marker("P").into(),
        }],
    )
    .expect("the private result stashes");
    slot.respond(
        vec![result("call-s1", "session_tool", &marker("S"))],
        &cancel,
    )
    .await
    .expect("the session cover resumes");

    let last = fanout_step(&state, session, Some(owner.downgrade()), &request, &cancel).await;
    assert!(ends_in_turn(&last), "the turn ends: {last:?}");
    assert!(!slot.is_awaiting());
    assert!(owner.release_if_live());

    assert_eq!(
        server.wire.run_count(),
        1,
        "owner-carrying fanout inference reuses the one durable run"
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("P")).await,
        1,
        "the stashed result is delivered once"
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker("S")).await,
        1,
        "the session result is delivered once"
    );
    assert!(server.wire.h2_only(), "every request spoke HTTP/2");
    server.shutdown().await;
}

/// A gone owner never resurrects its run: a dropped owner fails the request
/// with the typed dead-owner error, and a released slot fails with the typed
/// released error. Neither falls back to a silent one-shot open, so no
/// handshake reaches the wire in either case.
#[tokio::test]
async fn released_owner_never_resurrects_the_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(text_frame("a silent one-shot must never open")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let (state, _host) = fanout_host(dir.path(), &server.base_url());
    let session = SessionId::new_v7();
    let cancel = CancellationToken::new();
    let request = cursor_request(&["session_tool"]);

    let dead = {
        let owner = ContinuationOwner::new(SessionTasks::new());
        owner.downgrade()
    };
    let gone = fanout_step(&state, session, Some(dead), &request, &cancel).await;
    assert!(
        matches!(
            &gone[..],
            [Err(ProviderError::InvalidRequest { message })]
                if message.contains("the continuation owner is gone")
        ),
        "a dropped owner is a typed error, never a silent one-shot: {gone:?}"
    );

    let owner = ContinuationOwner::new(SessionTasks::new());
    assert!(
        !owner.release_if_live(),
        "releasing an idle owner releases nothing"
    );
    let ended = fanout_step(&state, session, Some(owner.downgrade()), &request, &cancel).await;
    assert!(
        matches!(
            &ended[..],
            [Err(ProviderError::InvalidRequest { message })]
                if message.contains("the continuation is released")
        ),
        "a released slot is a typed error, never a silent one-shot: {ended:?}"
    );

    assert_eq!(
        server.wire.run_count(),
        0,
        "no handshake leaves while the owner is gone"
    );
    server.shutdown().await;
}
