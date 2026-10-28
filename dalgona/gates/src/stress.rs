// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Shared, opt-in orchestration stress workload.
//!
//! This module owns the real Dalgona factory workload used by orchestration-01:
//! a loopback OpenAI-chat/SSE provider, 500 child runs, 200 detached exec
//! jobs, and eight public WebSocket observers.  It deliberately uses the
//! public dal-wire transport rather than a private server/client seam.

#![expect(
    clippy::disallowed_methods,
    reason = "stress gates intentionally use loopback sockets, tempdirs, and process probes"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use dal_core::{Command, Expect, PageReq, Part, UpdateKind};
use dal_wire::{WebSocketTransport, router::RouterOptions};
use sonic_rs::{JsonContainerTrait as _, JsonValueMutTrait as _, JsonValueTrait as _};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::support::{self, TestResult};

const CHILD_COUNT: usize = 500;
const JOB_COUNT: usize = 200;
const WORKERS: usize = 64;
const OBSERVER_COUNT: usize = 8;
const RSS_LIMIT_MIB: u64 = 256;
const FD_LIMIT: usize = 2_048;
// Windows `HandleCount` covers thread handles while `/dev/fd` never did;
// tokio also keeps blocking-pool threads and parked driver handles alive
// for the runtime's whole life, so the Windows check below subtracts
// thread growth instead of comparing raw totals.
#[cfg(windows)]
const RUNTIME_HANDLE_SLACK: usize = 64;
const CANCEL_SAMPLES: usize = 100;
const CANCEL_P99: Duration = Duration::from_millis(250);
const KEY_SAMPLES: usize = 100;
const KEY_P99: Duration = Duration::from_millis(24);
const JOB_SLEEP_S: u64 = 700;
const PROVIDER_LIMIT: usize = 64;
const DELIVERY_WINDOW: Duration = Duration::from_secs(3);
const WAIT_LIMIT: Duration = Duration::from_secs(180);
const MODEL: &str = "openai/gpt-5.6-luna";
const CLIENT: &str = "orchestration-stress-gate";

/// Which authored stress criterion is being run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Criterion {
    /// The complete process, descriptor, replay, WS, and provider contract.
    Composite,
    /// Idle and loaded cancellation distributions.
    Cancel,
    /// Full workload RSS cap.
    Rss,
    /// Full workload descriptor cap and return-to-baseline.
    Fds,
    /// Full-workload PTY keypress echo distribution.
    Keypress,
}

/// Counters from the content-addressed provider.  They are checked after the
/// host settles so a test cannot pass merely because the downstream journal is
/// empty.
#[derive(Debug, Default)]
struct ProviderState {
    requests: AtomicUsize,
    parent_answers: AtomicUsize,
    child_answers: AtomicUsize,
    job_answers: AtomicUsize,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

struct InFlightGuard<'a> {
    state: &'a ProviderState,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ProviderState {
    fn begin(&self) -> InFlightGuard<'_> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let live = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(live, Ordering::SeqCst);
        InFlightGuard { state: self }
    }
}

struct ProviderServer {
    state: Arc<ProviderState>,
    stop: dal_agent::login::CancellationToken,
    supervisor: Option<tokio::task::JoinHandle<TestResult<()>>>,
}

impl ProviderServer {
    async fn start(workspace: &Path) -> TestResult<(Self, String)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let state = Arc::new(ProviderState::default());
        let workspace = workspace.to_path_buf();
        let shared = Arc::clone(&state);
        let stop = dal_agent::login::CancellationToken::new();
        let supervisor_stop = stop.clone();
        let supervisor = tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(PROVIDER_LIMIT));
            let mut tasks = JoinSet::new();
            loop {
                let can_accept = tasks.len() < PROVIDER_LIMIT;
                let has_tasks = !tasks.is_empty();
                tokio::select! {
                    biased;
                    () = supervisor_stop.cancelled() => {
                        tasks.abort_all();
                        while let Some(result) = tasks.join_next().await {
                            if let Err(error) = result
                                && !error.is_cancelled()
                            {
                                return Err(format!("provider task join failed during stop: {error}").into());
                            }
                        }
                        return Ok(());
                    }
                    accepted = listener.accept(), if can_accept => {
                        let (stream, _) = accepted
                            .map_err(|error| format!("loopback provider accept failed: {error}"))?;
                        let permit = permits
                            .clone()
                            .acquire_owned()
                            .await
                            .map_err(|_| "loopback provider permit set closed")?;
                        let state = Arc::clone(&shared);
                        let workspace = workspace.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            serve_provider(stream, &state, &workspace).await
                        });
                    }
                    result = tasks.join_next(), if has_tasks => {
                        match result {
                            Some(Ok(Ok(()))) | None => {}
                            Some(Ok(Err(error))) => {
                                tasks.abort_all();
                                while tasks.join_next().await.is_some() {}
                                return Err(error);
                            }
                            Some(Err(error)) => {
                                if !error.is_cancelled() {
                                    tasks.abort_all();
                                    while tasks.join_next().await.is_some() {}
                                    return Err(format!("provider task join failed: {error}").into());
                                }
                            }
                        }
                    }
                }
            }
        });
        Ok((
            Self {
                state,
                stop,
                supervisor: Some(supervisor),
            },
            format!("http://{address}"),
        ))
    }

    async fn stop(&mut self) -> TestResult<()> {
        self.stop.cancel();
        if let Some(supervisor) = self.supervisor.take() {
            supervisor
                .await
                .map_err(|error| format!("provider supervisor join failed: {error}"))??;
        }
        Ok(())
    }
}

async fn read_request(
    stream: tokio::net::TcpStream,
) -> TestResult<(String, Vec<u8>, tokio::net::TcpStream)> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
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
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0_u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok((target, body, reader.into_inner()))
}

fn first_user(body: &sonic_rs::Value) -> String {
    let Some(messages) = body.get("messages").and_then(|value| value.as_array()) else {
        return String::new();
    };
    messages
        .iter()
        .find(|message| message.get("role").and_then(|value| value.as_str()) == Some("user"))
        .and_then(|message| message.get("content"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn tools(body: &sonic_rs::Value) -> Vec<String> {
    let Some(items) = body.get("tools").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            item.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .collect()
}

fn answered_tool(body: &sonic_rs::Value, name: &str) -> bool {
    let answered: BTreeSet<String> = body
        .get("messages")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(|value| value.as_str()) == Some("tool"))
        .filter_map(|message| message.get("tool_call_id"))
        .filter_map(|value| value.as_str())
        .map(str::to_owned)
        .collect();
    body.get("messages")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(|value| value.as_str()) == Some("assistant"))
        .filter_map(|message| message.get("tool_calls"))
        .filter_map(|value| value.as_array())
        .flatten()
        .any(|call| {
            call.get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                == Some(name)
                && call
                    .get("id")
                    .and_then(|value| value.as_str())
                    .is_some_and(|id| answered.contains(id))
        })
}

fn json_string(text: &str) -> String {
    support::json_text(text)
}

fn sse_text(text: &str) -> String {
    let usage = r#"{"prompt_tokens":1,"completion_tokens":1}"#;
    format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{}}}}}]}}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{usage}}}\n\ndata: [DONE]\n\n",
        json_string(text)
    )
}

fn sse_calls(calls: &[(String, String, String)]) -> String {
    use std::fmt::Write as _;
    let mut output = String::new();
    for (index, (id, name, args)) in calls.iter().enumerate() {
        let _ = writeln!(
            output,
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":{index},\"id\":{},\"function\":{{\"name\":{},\"arguments\":{}}}}}]}}}}]}}\n",
            json_string(id),
            json_string(name),
            json_string(args)
        );
    }
    let _ = write!(
        output,
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1}}}}\n\ndata: [DONE]\n\n"
    );
    output
}

fn parent_calls(workspace: &Path) -> Vec<(String, String, String)> {
    let items = (0..CHILD_COUNT)
        .map(|index| format!(r#""child-{index:04}""#))
        .collect::<Vec<_>>()
        .join(",");
    let mut calls = Vec::with_capacity(JOB_COUNT + 1);
    calls.push((
        "pool".to_owned(),
        "agents".to_owned(),
        format!(
            r#"{{"action":"run","name":"composite-pool","steps":[{{"name":"map","items":[{items}],"workers":{WORKERS},"prompt":"process {{{{item}}}} and report"}}]}}"#
        ),
    ));
    for index in 0..JOB_COUNT {
        let file = support::report_file(
            &workspace.join(support::STRESS_PIDS_DIR),
            &format!("job-{index:03}"),
        );
        let command = support::tree_report_command(
            &file,
            JOB_SLEEP_S,
            &format!("echo stress-job-{index}; exec sleep {JOB_SLEEP_S}"),
        );
        let args = format!(
            r#"{{"command":{},"foreground_s":1}}"#,
            json_string(&command)
        );
        calls.push((format!("job-{index:03}"), "exec".to_owned(), args));
    }
    calls
}

async fn provider_response(
    body: &[u8],
    state: &ProviderState,
    workspace: &Path,
) -> TestResult<String> {
    let parsed: sonic_rs::Value = sonic_rs::from_slice(body)?;
    let advertised = tools(&parsed);
    let prompt = first_user(&parsed);
    if prompt.contains("cancel-probe") {
        tokio::time::sleep(Duration::from_millis(15)).await;
        return Ok(sse_text("cancel probe"));
    }
    if advertised
        .iter()
        .any(|name| name == "orchestration__report")
    {
        if answered_tool(&parsed, "orchestration__report") {
            return Ok(sse_text("reported"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        state.child_answers.fetch_add(1, Ordering::SeqCst);
        return Ok(sse_calls(&[(
            "report".to_owned(),
            "orchestration__report".to_owned(),
            r#"{"status":"done","report":"processed"}"#.to_owned(),
        )]));
    }
    if advertised.iter().any(|name| name == "agents") {
        if answered_tool(&parsed, "agents") {
            return Ok(sse_text("started"));
        }
        state.parent_answers.fetch_add(1, Ordering::SeqCst);
        state.job_answers.fetch_add(JOB_COUNT, Ordering::SeqCst);
        return Ok(sse_calls(&parent_calls(workspace)));
    }
    if advertised.iter().any(|name| name == "exec") {
        if !answered_tool(&parsed, "exec") {
            state.job_answers.fetch_add(1, Ordering::SeqCst);
        }
        return Ok(sse_text("started"));
    }
    Ok(sse_text("ready"))
}

async fn serve_provider(
    stream: tokio::net::TcpStream,
    state: &ProviderState,
    workspace: &Path,
) -> TestResult<()> {
    let (target, body, mut stream) = read_request(stream).await?;
    if !target.contains("chat/completions") {
        stream
            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let _request = state.begin();
    let events = provider_response(&body, state, workspace).await?;
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
        events.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    Ok(())
}

/// Threads live in this process. Windows `HandleCount` covers thread
/// handles while Unix `/dev/fd` never did, so the Windows baseline check
/// subtracts thread growth instead of comparing raw totals.
///
/// # Errors
/// Returns the spawn or parse failure of the Windows thread counter.
#[cfg(windows)]
fn open_thread_count() -> std::io::Result<usize> {
    let command = format!(
        "(Get-Process -Id {} | Select-Object -ExpandProperty Threads).Count",
        std::process::id()
    );
    let output = support::run_command(std::process::Command::new("powershell.exe").args([
        "-NoProfile",
        "-Command",
        &command,
    ]))?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<usize>()
        .map_err(std::io::Error::other)
}

struct Harness {
    root: PathBuf,
    data: PathBuf,
    workspace: PathBuf,
    reports: PathBuf,
    provider: ProviderServer,
    host: Option<dal_agent::Host>,
    serve_stop: dal_agent::login::CancellationToken,
    serve: Option<dal_wire::ServeHandle>,
    baseline: Option<usize>,
    #[cfg(windows)]
    baseline_threads: Option<usize>,
    session: Option<dal_core::SessionId>,
    agent: Option<dal_agent::Agent>,
    sampler: Option<support::PeakSampler>,
    user: String,
}

impl Harness {
    async fn new_with_router(label: &str) -> TestResult<Self> {
        Self::new_inner(label, true).await
    }

    async fn new_idle(label: &str) -> TestResult<Self> {
        Self::new_inner(label, false).await
    }

    async fn new_inner(label: &str, with_router: bool) -> TestResult<Self> {
        let root =
            std::env::temp_dir().join(format!("dalgona-stress-{label}-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let workspace = root.join("workspace");
        let reports = workspace.join(support::STRESS_PIDS_DIR);
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&reports)?;
        let (provider, base_url) = ProviderServer::start(&workspace).await?;
        let user = format!(
            "model = \"{MODEL}\"\napproval = \"all\"\n\n[judge]\ngate = \"off\"\n\n[plugin.orchestration.agents]\nmax_runs = {WORKERS}\n\n[providers.openai]\nbase_url = \"{base_url}\"\napi = \"openai_chat\"\nmax_concurrent_requests = {PROVIDER_LIMIT}\n"
        );
        support::grant_bundled_extensions(&data, &workspace, &user, CLIENT).await?;
        let product = support::build_product(data.clone(), Some(&user))?;
        let mut vars = BTreeMap::new();
        vars.insert("OPENAI_API_KEY".into(), "sk-loopback".into());
        let env = dal_agent::Env {
            vars,
            cwd: workspace.clone(),
            sandbox_helper: None,
        };
        let config = support::host_config(&data, &user)?;
        let host = dal_agent::Host::start(product, config, env).await?;
        let serve_stop = dal_agent::login::CancellationToken::new();
        let serve = if with_router {
            let options = RouterOptions {
                bind: "127.0.0.1".to_owned(),
                port: 0,
                public: false,
                a2a: false,
                token_file: data.join("serve.token"),
                approval: dal_core::ApprovalMode::All,
                origins: Vec::new(),
                aliases: BTreeMap::new(),
                workspace: workspace.clone(),
            };
            Some(dal_wire::serve_router(host.clone(), options, serve_stop.clone()).await?)
        } else {
            None
        };
        let baseline = if with_router {
            Some(support::open_handle_count()?)
        } else {
            None
        };
        #[cfg(windows)]
        let baseline_threads = if with_router {
            Some(open_thread_count()?)
        } else {
            None
        };
        let sampler = Some(support::PeakSampler::start(Duration::from_secs(1)));
        Ok(Self {
            root,
            data,
            workspace,
            reports,
            provider,
            host: Some(host),
            serve_stop,
            serve,
            baseline,
            #[cfg(windows)]
            baseline_threads,
            session: None,
            agent: None,
            sampler,
            user,
        })
    }

    async fn open(&mut self, name: &str) -> TestResult<dal_agent::Agent> {
        let host = self.host.as_ref().ok_or("host already shut down")?;
        let agent = host
            .open(
                dal_agent::SessionRef::New {
                    workspace: dal_core::Workspace::new(self.workspace.clone())?,
                    name: None,
                },
                dal_core::ClientId::new(name),
            )
            .await?;
        self.session = Some(agent.session());
        self.agent = Some(agent.clone());
        Ok(agent)
    }

    async fn stop(&mut self) -> TestResult<StopReport> {
        let session = self.session.take();
        if let (Some(host), Some(session)) = (self.host.as_ref(), session) {
            let _ = host.close(session).await;
        }
        let host = self.host.take().ok_or("host already shut down")?;
        let shutdown = host.shutdown(Duration::from_secs(10)).await;
        if shutdown.tasks_remaining != 0 {
            return Err(format!("{} host tasks remained", shutdown.tasks_remaining).into());
        }
        self.serve_stop.cancel();
        if let Some(serve) = self.serve.take() {
            let _ = serve.wait().await;
        }
        let counters = self.provider.state.clone();
        self.provider.stop().await?;
        let peak = if let Some(sampler) = self.sampler.take() {
            tokio::task::spawn_blocking(move || sampler.finish())
                .await
                .map_err(|join| format!("sampler task failed: {join}"))??
        } else {
            support::ResourcePeak::default()
        };
        let baseline_ok = if let Some(baseline) = self.baseline {
            #[cfg(not(windows))]
            {
                let after = support::open_handle_count()?;
                if after != baseline {
                    return Err(format!(
                        "open descriptors returned to {after}, baseline was {baseline}"
                    )
                    .into());
                }
                true
            }
            #[cfg(windows)]
            {
                let baseline_threads = self.baseline_threads.unwrap_or(0);
                let mut after_handles = support::open_handle_count()?;
                let mut after_threads = open_thread_count()?;
                let settled = |handles: usize, threads: usize| {
                    handles.saturating_sub(baseline)
                        <= threads.saturating_sub(baseline_threads) + RUNTIME_HANDLE_SLACK
                };
                let deadline = Instant::now() + Duration::from_secs(20);
                while !settled(after_handles, after_threads) && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    after_handles = support::open_handle_count()?;
                    after_threads = open_thread_count()?;
                }
                let growth = after_handles.saturating_sub(baseline);
                let thread_growth = after_threads.saturating_sub(baseline_threads);
                if growth > thread_growth + RUNTIME_HANDLE_SLACK {
                    return Err(format!(
                        "non-thread kernel handles did not return to baseline \
                         (handles={after_handles} baseline={baseline} \
                         threads={after_threads} baseline-threads={baseline_threads})"
                    )
                    .into());
                }
                true
            }
        } else {
            true
        };
        let root = self.root.clone();
        let _ = std::fs::remove_dir_all(root);
        Ok(StopReport {
            peak,
            baseline_ok,
            requests: counters.requests.load(Ordering::SeqCst),
            parent_answers: counters.parent_answers.load(Ordering::SeqCst),
            child_answers: counters.child_answers.load(Ordering::SeqCst),
            job_answers: counters.job_answers.load(Ordering::SeqCst),
            provider_peak: counters.peak_in_flight.load(Ordering::SeqCst),
        })
    }
}

struct StopReport {
    peak: support::ResourcePeak,
    baseline_ok: bool,
    requests: usize,
    parent_answers: usize,
    child_answers: usize,
    job_answers: usize,
    provider_peak: usize,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.serve_stop.cancel();
        self.provider.stop.cancel();
        if let Some(supervisor) = self.provider.supervisor.take() {
            supervisor.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Default)]
struct ObserverResult {
    deliveries: usize,
    relevant: usize,
    resyncs: usize,
    duplicates: usize,
    last: Option<(u64, u64)>,
    reattached: bool,
}

async fn read_json(ws: &mut WebSocketTransport) -> TestResult<sonic_rs::Value> {
    Ok(sonic_rs::from_str(&ws.read_frame().await?)?)
}

async fn wait_reply(
    ws: &mut WebSocketTransport,
    id: u64,
    result: &mut ObserverResult,
) -> TestResult<()> {
    loop {
        let value = read_json(ws).await?;
        if value.get("method").and_then(|value| value.as_str()) == Some("session/update") {
            record_update(&value, result);
            continue;
        }
        if value
            .get("id")
            .and_then(sonic_rs::JsonValueTrait::as_i64)
            .and_then(|number| u64::try_from(number).ok())
            == Some(id)
        {
            if value.get("error").is_some() {
                return Err(format!("wire request {id} failed: {value}").into());
            }
            return Ok(());
        }
    }
}

fn record_update(value: &sonic_rs::Value, result: &mut ObserverResult) {
    let Some(params) = value.get("params") else {
        return;
    };
    let generation = params
        .get("gen")
        .and_then(sonic_rs::JsonValueTrait::as_i64)
        .and_then(|number| u64::try_from(number).ok())
        .unwrap_or(0);
    let seq = params
        .get("seq")
        .and_then(sonic_rs::JsonValueTrait::as_i64)
        .and_then(|number| u64::try_from(number).ok())
        .unwrap_or(0);
    let pair = (generation, seq);
    if result.last.is_some_and(|last| pair <= last) {
        result.duplicates += 1;
    }
    result.last = Some(pair);
    result.deliveries += 1;
    if params
        .get("update")
        .and_then(|update| update.get("type"))
        .and_then(|value| value.as_str())
        .is_some_and(|kind| kind != "resync")
    {
        result.relevant += 1;
    } else {
        result.resyncs += 1;
    }
}

async fn subscribe_wire(
    ws: &mut WebSocketTransport,
    id: u64,
    session: &str,
    cursor: Option<(u64, u64)>,
    result: &mut ObserverResult,
) -> TestResult<()> {
    let mut params = sonic_rs::json!({"sessionId": session});
    if let Some((generation, seq)) = cursor
        && let Some(object) = params.as_object_mut()
    {
        object.insert("gen", sonic_rs::Value::from(generation));
        object.insert("after", sonic_rs::Value::from(seq));
    }
    let request =
        sonic_rs::json!({"jsonrpc":"2.0","id":id,"method":"session/subscribe","params":params});
    ws.write_frame(&sonic_rs::to_string(&request)?).await?;
    wait_reply(ws, id, result).await
}

async fn observer(url: String, session: String, index: usize) -> TestResult<ObserverResult> {
    let mut ws = WebSocketTransport::connect_with_auth(&url, "").await?;
    let mut result = ObserverResult::default();
    let initialize = sonic_rs::json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":1,"capabilities":["sessions"],"name":format!("stress-observer-{index}")}
    });
    ws.write_frame(&sonic_rs::to_string(&initialize)?).await?;
    wait_reply(&mut ws, 1, &mut result).await?;
    subscribe_wire(&mut ws, 2, &session, None, &mut result).await?;
    let started = Instant::now();
    let mut next_id = 3_u64;
    let mut reattached = false;
    while started.elapsed() < DELIVERY_WINDOW {
        if index == 0 && !reattached && started.elapsed() > Duration::from_millis(500) {
            let unsubscribe = sonic_rs::json!({
                "jsonrpc":"2.0","id":next_id,"method":"session/unsubscribe",
                "params":{"sessionId":session}
            });
            ws.write_frame(&sonic_rs::to_string(&unsubscribe)?).await?;
            wait_reply(&mut ws, next_id, &mut result).await?;
            next_id += 1;
            subscribe_wire(&mut ws, next_id, &session, result.last, &mut result).await?;
            next_id += 1;
            reattached = true;
        }
        if let Ok(frame) = tokio::time::timeout(Duration::from_millis(100), ws.read_frame()).await {
            let frame = frame?;
            let value: sonic_rs::Value = sonic_rs::from_str(&frame)?;
            if value.get("method").and_then(|value| value.as_str()) == Some("session/update") {
                record_update(&value, &mut result);
            }
        }
    }
    result.reattached = reattached;
    Ok(result)
}

async fn wait_reports_and_jobs(
    harness: &Harness,
    subscription: &mut dal_agent::Subscription,
) -> TestResult<BTreeSet<String>> {
    let reports = harness.reports.clone();
    tokio::task::spawn_blocking(move || {
        support::await_process_reports(&reports, JOB_COUNT, WAIT_LIMIT)
    })
    .await
    .map_err(|join| format!("report reader failed: {join}"))??;
    let mut jobs = BTreeSet::new();
    while jobs.len() < JOB_COUNT {
        let delivery = subscription
            .next()
            .await
            .ok_or("workload subscription ended")?;
        if let dal_agent::Delivery::Update(update) = delivery
            && let UpdateKind::ToolSettled { outcome, .. } = &update.kind
        {
            let text = outcome.text.to_string();
            if let Some(id) = text.strip_prefix("detached as job ") {
                jobs.insert(id.trim().to_owned());
            }
        }
    }
    Ok(jobs)
}

/// One journal page fingerprint: generation, sequence, entry ids, and cursor.
type PageFingerprint = (u64, u64, Vec<dal_core::EntryId>, Option<dal_core::EntryId>);

fn page_fingerprint(agent: &dal_agent::Agent) -> TestResult<Vec<PageFingerprint>> {
    let mut pages = Vec::new();
    let mut before = None;
    loop {
        let page = agent.view(PageReq::new(
            std::num::NonZeroU32::new(1_000).ok_or("page window must be non-zero")?,
            before,
        )?)?;
        let ids = page
            .entries
            .items
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        let next = page.entries.next_before;
        pages.push((page.r#gen.get(), page.seq.get(), ids, next));
        if next.is_none() {
            break;
        }
        before = next;
    }
    Ok(pages)
}

async fn wait_child_count(state: &ProviderState) -> TestResult<()> {
    tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            if state.child_answers.load(Ordering::SeqCst) >= CHILD_COUNT {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "500 child sessions did not reach the provider".into())
}

async fn wait_ledger(
    data: &Path,
    session: dal_core::SessionId,
    expected: &BTreeSet<String>,
) -> TestResult<()> {
    let expected = expected.clone();
    tokio::time::timeout(WAIT_LIMIT, async {
        loop {
            if let Some(path) = support::session_ledger(data, &session.to_string())
                && let Ok(text) = std::fs::read_to_string(path)
            {
                let mut ends = BTreeSet::new();
                let mut end_count = 0usize;
                for line in text.lines() {
                    let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(line) else {
                        continue;
                    };
                    if value.get("e").and_then(|value| value.as_str()) == Some("end")
                        && let Some(id) = value.get("id").and_then(|value| value.as_str())
                    {
                        end_count += 1;
                        ends.insert(id.to_owned());
                    }
                }
                if end_count == expected.len() && ends == expected {
                    return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "job ledger did not settle the exact started set")?
}

enum WorkloadMode {
    Composite,
    LoadedCancel,
    Keypress,
}

fn spawn_observers(
    url: &str,
    session: dal_core::SessionId,
) -> Vec<tokio::task::JoinHandle<TestResult<ObserverResult>>> {
    let mut observers = Vec::with_capacity(OBSERVER_COUNT);
    for index in 0..OBSERVER_COUNT {
        observers.push(tokio::spawn(observer(
            url.to_owned(),
            session.to_string(),
            index,
        )));
    }
    observers
}

async fn probe_loaded_cancel(harness: &Harness) -> TestResult<Vec<Duration>> {
    let timing = harness
        .host
        .as_ref()
        .ok_or("host already shut down")?
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(harness.workspace.clone())?,
                name: None,
            },
            dal_core::ClientId::new("loaded-cancel-client"),
        )
        .await?;
    measure_cancel(&timing).await
}

async fn probe_keypress(harness: &Harness, connect: &str) -> TestResult<Vec<Duration>> {
    let binary = support::dalgona_binary()?;
    let fixture = harness.workspace.join("tui-script.jsonl");
    std::fs::write(&fixture, "")?;
    let config = harness.workspace.join("dal.toml");
    std::fs::write(
        &config,
        format!(
            "model = \"{MODEL}\"\n\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    crate::pty::keypress_echo_latencies(
        &binary,
        &["--connect", connect],
        &[
            ("TERM", "xterm-256color"),
            ("NO_COLOR", "1"),
            ("DAL_NO_MOTION", "1"),
            (
                "HOME",
                harness.workspace.to_str().ok_or("workspace is not UTF-8")?,
            ),
        ],
        &harness.workspace,
        KEY_SAMPLES,
        Duration::from_millis(15),
        Duration::from_millis(500),
    )
    .await
}

async fn cancel_jobs_and_reap(
    agent: &dal_agent::Agent,
    jobs: &BTreeSet<String>,
    reported: &[support::ReportedProcess],
) -> TestResult<()> {
    let cancel_at = Instant::now();
    for id in jobs {
        let encoded = format!("\"{id}\"");
        let job: dal_core::JobId = sonic_rs::from_str(&encoded)?;
        agent
            .submit(Command::Cancel {
                scope: dal_core::CancelScope::Job(job),
            })
            .await?;
    }
    let reported = reported.to_vec();
    let gone = tokio::task::spawn_blocking(move || {
        support::wait_until_gone(&reported, cancel_at, Duration::from_secs(10))
    })
    .await
    .map_err(|join| format!("process census failed: {join}"))??;
    if gone > Instant::now() {
        return Err("process census returned a future timestamp".into());
    }
    Ok(())
}

async fn verify_replay(
    harness: &Harness,
    session: dal_core::SessionId,
    live_pages: &[PageFingerprint],
) -> TestResult<()> {
    let replay_product = support::build_product(harness.data.clone(), Some(&harness.user))?;
    let replay_config = support::host_config(&harness.data, &harness.user)?;
    let replay_env = dal_agent::Env {
        vars: {
            let mut vars = BTreeMap::new();
            vars.insert("OPENAI_API_KEY".into(), "sk-loopback".into());
            vars
        },
        cwd: harness.workspace.clone(),
        sandbox_helper: None,
    };
    let replay_host = dal_agent::Host::start(replay_product, replay_config, replay_env).await?;
    let replay_agent = replay_host
        .open(
            dal_agent::SessionRef::Resume {
                key: session.to_string().into(),
                workspace: dal_core::Workspace::new(harness.workspace.clone())?,
            },
            dal_core::ClientId::new("replay-stress-client"),
        )
        .await?;
    let replay_pages = page_fingerprint(&replay_agent)?;
    if replay_pages.as_slice() != live_pages {
        return Err("journal replay view differs from the live paged view".into());
    }
    replay_host.close(replay_agent.session()).await?;
    let replay_report = replay_host.shutdown(Duration::from_secs(10)).await;
    if replay_report.tasks_remaining != 0 {
        return Err("replay host retained tasks after shutdown".into());
    }
    Ok(())
}

async fn join_and_check_observers(
    observers: Vec<tokio::task::JoinHandle<TestResult<ObserverResult>>>,
) -> TestResult<()> {
    let mut observer_results = Vec::with_capacity(observers.len());
    for observer in observers {
        observer_results.push(
            observer
                .await
                .map_err(|join| format!("observer join failed: {join}"))??,
        );
    }
    for (index, result) in observer_results.iter().enumerate() {
        if result.deliveries == 0
            || result.relevant == 0
            || result.resyncs != 0
            || result.duplicates != 0
        {
            return Err(format!("observer {index} did not deliver a bounded, duplicate-free update stream: {result:?}").into());
        }
        if index == 0 && !result.reattached {
            return Err("the reattach observer never reattached from its last cursor".into());
        }
    }
    Ok(())
}

fn check_provider_and_resource_bounds(report: &StopReport) -> TestResult<()> {
    if report.requests == 0
        || report.parent_answers == 0
        || report.child_answers != CHILD_COUNT
        || report.job_answers != JOB_COUNT
    {
        return Err(format!(
            "provider counters did not prove workload: requests={} parent={} children={} jobs={}",
            report.requests, report.parent_answers, report.child_answers, report.job_answers
        )
        .into());
    }
    if report.provider_peak > PROVIDER_LIMIT {
        return Err(format!(
            "provider peak {} exceeded {PROVIDER_LIMIT}",
            report.provider_peak
        )
        .into());
    }
    if report.peak.resident_bytes / (1024 * 1024) >= RSS_LIMIT_MIB {
        return Err(format!(
            "peak RSS {} MiB exceeded {RSS_LIMIT_MIB}",
            report.peak.resident_bytes / (1024 * 1024)
        )
        .into());
    }
    if report.peak.open_handles > FD_LIMIT {
        return Err(format!(
            "peak descriptors {} exceeded {FD_LIMIT}",
            report.peak.open_handles
        )
        .into());
    }
    Ok(())
}

async fn full_workload(
    mode: WorkloadMode,
) -> TestResult<(StopReport, Option<Vec<Duration>>, Option<Vec<Duration>>)> {
    let with_keypress = matches!(mode, WorkloadMode::Keypress);
    let with_loaded_cancel = matches!(mode, WorkloadMode::LoadedCancel);
    let mut harness = Harness::new_with_router("composite").await?;
    let agent = harness.open(CLIENT).await?;
    let session = agent.session();
    let mut updates = agent.subscribe_listen(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "run composite 500 children and 200 jobs".into(),
            }],
        })
        .await?;
    let jobs = wait_reports_and_jobs(&harness, &mut updates).await?;
    let reported = support::read_process_reports(&harness.reports)?;
    let alive = support::assert_all_alive(&reported)?;
    if alive.pids.len() < JOB_COUNT * 2 {
        return Err(format!("only {} process ids were alive", alive.pids.len()).into());
    }
    let url = {
        let serve = harness.serve.as_ref().ok_or("WS server was not started")?;
        format!("ws://{}/v1/ws", serve.local_addr())
    };
    let observers = spawn_observers(&url, session);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let loaded_cancel = if with_loaded_cancel {
        Some(probe_loaded_cancel(&harness).await?)
    } else {
        None
    };
    let keypress = if with_keypress {
        Some(probe_keypress(&harness, &url).await?)
    } else {
        None
    };
    wait_child_count(&harness.provider.state).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel_jobs_and_reap(&agent, &jobs, &reported).await?;
    wait_ledger(&harness.data, session, &jobs).await?;
    let live_pages = page_fingerprint(&agent)?;
    harness
        .host
        .as_ref()
        .ok_or("host already shut down")?
        .close(session)
        .await?;
    verify_replay(&harness, session, &live_pages).await?;
    join_and_check_observers(observers).await?;
    let report = harness.stop().await?;
    check_provider_and_resource_bounds(&report)?;
    Ok((report, loaded_cancel, keypress))
}

async fn measure_cancel(agent: &dal_agent::Agent) -> TestResult<Vec<Duration>> {
    let mut subscription = agent.subscribe_listen(None)?;
    let mut samples = Vec::with_capacity(CANCEL_SAMPLES);
    for index in 0..CANCEL_SAMPLES {
        agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: format!("cancel-probe {index}").into(),
                }],
            })
            .await?;
        let turn = loop {
            let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
                .await
                .map_err(|_| "cancellation subscription timed out")?
                .ok_or("cancellation subscription ended")?;
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            if let UpdateKind::TurnStarted { turn, .. } = &update.kind {
                break *turn;
            }
        };
        let started = Instant::now();
        agent
            .submit(Command::Cancel {
                scope: dal_core::CancelScope::Turn(turn),
            })
            .await?;
        loop {
            let delivery = tokio::time::timeout(Duration::from_secs(10), subscription.next())
                .await
                .map_err(|_| "cancellation subscription timed out")?
                .ok_or("cancellation subscription ended")?;
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            if let UpdateKind::TurnEnded { turn: ended, .. } = &update.kind
                && *ended == turn
            {
                samples.push(started.elapsed());
                break;
            }
        }
    }
    Ok(samples)
}

async fn idle_cancel() -> TestResult<Vec<Duration>> {
    let mut harness = Harness::new_idle("idle-cancel").await?;
    let agent = harness.open("idle-cancel-client").await?;
    let samples = measure_cancel(&agent).await?;
    let _ = harness.stop().await?;
    Ok(samples)
}

fn check_p99(samples: &[Duration], bound: Duration, label: &str) -> TestResult<()> {
    if samples.len() != CANCEL_SAMPLES && label.starts_with("cancel") {
        return Err(format!(
            "{label}: expected {CANCEL_SAMPLES} samples, got {}",
            samples.len()
        )
        .into());
    }
    let p99 = support::nearest_rank_p99(samples)?;
    if p99 >= bound {
        return Err(format!("{label}: p99 {p99:?} is not below {bound:?}").into());
    }
    Ok(())
}

/// Runs the complete process/WS/replay workload.
///
/// # Errors
///
/// Returns an error if the 500-child and 200-job workload, the eight
/// observers, the journal replay comparison, or the provider, RSS, or
/// descriptor bounds fail.
pub async fn run_composite() -> TestResult<()> {
    let _ = full_workload(WorkloadMode::Composite).await?;
    Ok(())
}

/// Runs 100 idle and 100 full-workload cancellation samples.
///
/// # Errors
///
/// Returns an error if either cancellation distribution collects the wrong
/// sample count or reaches the 250 ms p99 bound, or if the hosts fail.
pub async fn run_cancel_p99() -> TestResult<()> {
    let idle = idle_cancel().await?;
    check_p99(&idle, CANCEL_P99, "cancel-p99-idle100")?;
    let (_, loaded, _) = full_workload(WorkloadMode::LoadedCancel).await?;
    let loaded = loaded.ok_or("loaded cancellation samples were not collected")?;
    check_p99(&loaded, CANCEL_P99, "cancel-p99-loaded100")?;
    Ok(())
}

/// Runs the full workload and checks the authored RSS bound.
///
/// # Errors
///
/// Returns an error if the full workload fails or peak RSS reaches
/// the 256 MiB cap.
pub async fn run_rss() -> TestResult<()> {
    let report = full_workload(WorkloadMode::Composite).await?.0;
    if report.peak.resident_bytes / (1024 * 1024) >= RSS_LIMIT_MIB {
        return Err(format!(
            "RSS cap exceeded: {} MiB",
            report.peak.resident_bytes / (1024 * 1024)
        )
        .into());
    }
    Ok(())
}

/// Runs the full workload and checks the authored descriptor bound.
///
/// # Errors
///
/// Returns an error if the full workload fails, peak descriptors exceed
/// 2,048, or open handles do not return to baseline.
pub async fn run_fds() -> TestResult<()> {
    let report = full_workload(WorkloadMode::Composite).await?.0;
    if report.peak.open_handles > FD_LIMIT || !report.baseline_ok {
        return Err(format!(
            "descriptor contract failed: peak {}, baseline={}",
            report.peak.open_handles, report.baseline_ok
        )
        .into());
    }
    Ok(())
}

/// Runs the real TUI through a PTY while the full workload is active.
///
/// # Errors
///
/// Returns an error if the keypress probe collects the wrong sample count,
/// reaches the 24 ms p99 bound, or the full workload fails.
pub async fn run_keypress() -> TestResult<()> {
    let (_, _, keypress) = full_workload(WorkloadMode::Keypress).await?;
    let samples = keypress.ok_or("keypress probe returned no samples")?;
    if samples.len() != KEY_SAMPLES {
        return Err(format!(
            "keypress probe returned {} samples, expected {KEY_SAMPLES}",
            samples.len()
        )
        .into());
    }
    let p99 = support::nearest_rank_p99(&samples)?;
    if p99 >= KEY_P99 {
        return Err(format!("keypress p99 {p99:?} is not below {KEY_P99:?}").into());
    }
    Ok(())
}
