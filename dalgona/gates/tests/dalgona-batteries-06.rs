// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The registered `web_fetch` tool converts a loopback-served HTML fixture.
use dalgona_gates::support;

use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{ChildStdin, ChildStdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Every battery other than `web` stays off so the scripted fetch below drives
/// exactly the registered web tools.
const WEB_ONLY: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"work\"]";
/// Bounded RPC startup and per-call budgets; every child is owned and waited.
const RPC_START: Duration = Duration::from_secs(30);
const RPC_CALL: Duration = Duration::from_secs(30);
const RPC_TURN: Duration = Duration::from_secs(90);

/// The live service-Grant handshake the positive fetch approved, session-only.
/// `capabilities` is exactly the declared web list (`net`+`env`, no more);
/// `approve_for_session` is session-memory-only, dies with the store.
/// Read-only `web_fetch` (`ToolClass::Read` → `Gate::ReadOnly` → `Allow`
/// in every mode, including `ask`) never opens a tool `Approval` question;
/// the earlier required-`Approval` fixture constraint was wrong, not a
/// product bug. Temp-root only.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GrantProof {
    request_id: String,
    ext: String,
    origin: String,
    capabilities: Vec<String>,
}
const FIXTURE_BODY: &[u8] = b"<h1>Hello</h1>";
const PUBLIC_FIXTURE_HOST: &str = "1.1.1.1";
const REQUEST_CAP: usize = 16 * 1024;
const PROXY_WAIT: Duration = Duration::from_secs(120);

/// One bounded, loopback-only HTTP proxy. It never opens an outbound socket:
/// it records the absolute target and returns the fixed fixture directly.
struct LoopbackFixtureProxy {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<Result<Option<ProxyRequest>, String>>>,
}

#[derive(Debug, PartialEq, Eq)]
struct ProxyRequest {
    method: String,
    target: String,
}

impl LoopbackFixtureProxy {
    fn spawn() -> support::TestResult<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = Arc::clone(&stop);
        let task = thread::spawn(move || {
            let deadline = Instant::now() + PROXY_WAIT;
            loop {
                if task_stop.load(Ordering::Acquire) {
                    return Ok(None);
                }
                match listener.accept() {
                    Ok((stream, _peer)) => return serve_fixture(stream).map(Some),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err("timed out waiting for the loopback fixture request".into());
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(format!("loopback fixture accept failed: {error}")),
                }
            }
        });
        Ok(Self {
            addr,
            stop,
            task: Some(task),
        })
    }

    fn finish(mut self) -> support::TestResult<Option<ProxyRequest>> {
        self.stop.store(true, Ordering::Release);
        self.task
            .take()
            .ok_or("loopback fixture task was already joined")?
            .join()
            .map_err(|_| "loopback fixture task panicked".to_owned())?
            .map_err(Into::into)
    }
}

impl Drop for LoopbackFixtureProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(task) = self.task.take() else {
            return;
        };
        if let Ok(mut wake) = TcpStream::connect(self.addr) {
            let _ = wake.write_all(b"\r\n\r\n");
        }
        let _ = task.join();
    }
}

fn serve_fixture(mut stream: TcpStream) -> Result<ProxyRequest, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("set fixture read timeout: {error}"))?;
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("read fixture request: {error}"))?;
        if read == 0 {
            return Err("fixture proxy received an incomplete request".into());
        }
        request.extend_from_slice(&chunk[..read]);
        if request.len() > REQUEST_CAP {
            return Err("fixture proxy request exceeded its cap".into());
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request_head = String::from_utf8(request).map_err(|_| "fixture request was not UTF-8")?;
    let request_line = request_head
        .lines()
        .next()
        .ok_or("fixture proxy received no request line")?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().ok_or("fixture request has no method")?;
    let target = fields.next().ok_or("fixture request has no target")?;
    let version = fields.next().ok_or("fixture request has no version")?;
    if fields.next().is_some() {
        return Err("fixture request line has extra fields".into());
    }
    if version != "HTTP/1.1" {
        return Err(format!("fixture request used {version}, not HTTP/1.1"));
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        FIXTURE_BODY.len()
    );
    stream
        .write_all(response.as_bytes())
        .and_then(|()| stream.write_all(FIXTURE_BODY))
        .map_err(|error| format!("write fixture response: {error}"))?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(ProxyRequest {
        method: method.to_owned(),
        target: target.to_owned(),
    })
}

fn tool_text(record: &str) -> support::TestResult<String> {
    let value: Value = sonic_rs::from_str(record.trim_end())?;
    value
        .get("parts")
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| "tool_result record carries no text part".into())
}

/// Proves the real web service-Grant handshake live in one binary RPC session.
///
/// Fail-closed, no auto-approval: `approval = "ask"` so the web Grant
/// (exactly the declared `net`+`env`) opens and
/// is answered live with `approve_for_session` (session-memory-only, dies
/// with the store). Only the `web` service Grant is approved; any tool
/// `Approval` is unexpected, declined fail-closed, and fails the test.
/// Proxy travels via the RPC child `Env` only (no process-global
/// writes). Uses a separate temp root (no sharing with the binary path, no
/// file transfer); returns tool text plus the recorded grant proof; the
/// caller finishes the proxy (by reference). Wire shapes follow the working
/// SDK witness (`initialize`, `session/open`, `session/subscribe`,
/// `session/submit`, `session/answer`, `session/update` with
/// `request_opened`/`turn_ended`) and the current `dal-core` schema
/// (`grant` question, `approve_for_session`/`decline` answers; an unexpected
/// `approval` question is declined and fails the test).
/// One bounded stdio JSONL RPC connection to the real `dalgona rpc` binary.
///
/// Owns the writer, the bounded frame receiver, the joined reader thread,
/// and the child guard so every pipe and task lifetime is owned. Drop order
/// is stdin close, receiver drop (unblock send), child kill and wait (force
/// EOF), then reader join; the reader is never joined before the pipes are
/// closed and the child is reaped.
struct RpcConn {
    writer: Option<ChildStdin>,
    frames: Option<mpsc::Receiver<Result<Value, String>>>,
    reader: Option<JoinHandle<()>>,
    child: Option<support::RpcChild>,
    next_id: u64,
    pending: Vec<Value>,
}

impl Drop for RpcConn {
    fn drop(&mut self) {
        drop(self.writer.take());
        drop(self.frames.take());
        drop(self.child.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn parse_stdio_line(text: &str) -> Result<Value, String> {
    sonic_rs::from_str::<Value>(text.trim_end())
        .map_err(|error| format!("RPC frame parse failed: {error}"))
}

fn forward_stdio_line(
    next: std::io::Result<String>,
    sender: &mpsc::SyncSender<Result<Value, String>>,
) -> bool {
    match next {
        Ok(text) => sender.send(parse_stdio_line(&text)).is_ok(),
        Err(error) => {
            let _ = sender.send(Err(format!("RPC stdio read failed: {error}")));
            false
        }
    }
}

fn spawn_stdio_reader(
    stdout: ChildStdout,
    sender: mpsc::SyncSender<Result<Value, String>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let lines = BufReader::new(stdout).lines();
        for next in lines {
            if !forward_stdio_line(next, &sender) {
                break;
            }
        }
    })
}

impl RpcConn {
    fn open(stdin: ChildStdin, stdout: ChildStdout, child: support::RpcChild) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1024);
        let reader = spawn_stdio_reader(stdout, sender);
        Self {
            writer: Some(stdin),
            frames: Some(receiver),
            reader: Some(reader),
            child: Some(child),
            next_id: 1,
            pending: Vec::new(),
        }
    }

    fn send(&mut self, method: &str, params: &Value) -> support::TestResult<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = sonic_rs::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = sonic_rs::to_string(&frame)?;
        line.push('\n');
        let writer = self
            .writer
            .as_mut()
            .ok_or("the RPC stdio writer is closed")?;
        writer.write_all(line.as_bytes())?;
        writer.flush()?;
        Ok(id)
    }

    fn recv_one(&mut self, budget: Duration) -> support::TestResult<Option<Value>> {
        if self.pending.len() > 1024 {
            return Err("RPC pending queue exceeded its bound; shedding".into());
        }
        let frames = self
            .frames
            .as_ref()
            .ok_or("the RPC stdio reader is closed")?;
        match frames.recv_timeout(budget) {
            Ok(Ok(frame)) => Ok(Some(frame)),
            Ok(Err(detail)) => Err(detail.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the RPC stdio closed".into()),
        }
    }

    fn call(
        &mut self,
        method: &str,
        params: &Value,
        budget: Duration,
    ) -> support::TestResult<Value> {
        let id = self.send(method, params)?;
        let deadline = Instant::now() + budget;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("{method} never answered within its budget").into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(message) = self.recv_one(remaining)? else {
                continue;
            };
            let is_reply = message.get("id").and_then(Value::as_u64) == Some(id)
                && message.get("method").and_then(Value::as_str).is_none();
            if is_reply {
                if let Some(error) = message.get("error") {
                    let detail = sonic_rs::to_string(error)?;
                    return Err(format!("{method} failed: {detail}").into());
                }
                let result = message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| format!("{method} returned no result"))?;
                return Ok(result);
            }
            if self.pending.len() >= 1024 {
                return Err("RPC pending queue exceeded its bound; shedding".into());
            }
            self.pending.push(message);
        }
    }

    fn drain(
        &mut self,
        kinds: &[&str],
        budget: Duration,
    ) -> support::TestResult<Vec<(Option<String>, Value)>> {
        let mut seen: Vec<(Option<String>, Value)> = Vec::new();
        let mut rest: Vec<Value> = Vec::new();
        let queued = std::mem::take(&mut self.pending);
        let mut queued: std::collections::VecDeque<Value> = queued.into();
        while let Some(message) = queued.pop_front() {
            let scoped = rpc_scoped(&message);
            let Some((owner, update)) = scoped else {
                rest.push(message);
                continue;
            };
            let kind = update.get("type").and_then(Value::as_str).unwrap_or("");
            let matched = kinds.contains(&kind);
            seen.push((owner, update));
            if matched {
                rest.extend(queued);
                self.pending = rest;
                return Ok(seen);
            }
        }
        self.pending = rest;
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(message) = self.recv_one(remaining)? else {
                break;
            };
            let scoped = rpc_scoped(&message);
            let Some((owner, update)) = scoped else {
                if self.pending.len() >= 1024 {
                    return Err("RPC pending queue exceeded its bound; shedding".into());
                }
                self.pending.push(message);
                continue;
            };
            let kind = update.get("type").and_then(Value::as_str).unwrap_or("");
            let matched = kinds.contains(&kind);
            seen.push((owner, update));
            if matched {
                return Ok(seen);
            }
        }
        Ok(seen)
    }
}

fn rpc_scoped(message: &Value) -> Option<(Option<String>, Value)> {
    if message.get("method").and_then(Value::as_str)? != "session/update" {
        return None;
    }
    let params = message.get("params")?;
    let update = params.get("update")?.clone();
    let owner = params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some((owner, update))
}

fn rpc_answer(
    conn: &mut RpcConn,
    session: &str,
    request_id: &str,
    answer_type: &str,
) -> support::TestResult<()> {
    let params = sonic_rs::json!({
        "sessionId": session,
        "requestId": request_id,
        "answer": { "type": answer_type },
    });
    conn.call("session/answer", &params, RPC_CALL)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedGrant {
    request_id: String,
    ext: String,
    origin: String,
    capabilities: Vec<String>,
}

struct GrantPump {
    conn: RpcConn,
    session: String,
    turn: u64,
    grant: Option<ObservedGrant>,
    unexpected_approval: Option<String>,
}

impl GrantPump {
    fn open(mut conn: RpcConn, workspace: &str) -> support::TestResult<Self> {
        let init_params = sonic_rs::json!({
            "protocolVersion": 1,
            "clientInfo": { "name": "dalgona-batteries-06", "version": "1" },
            "capabilities": ["sessions", "approval", "ask"],
        });
        let init = conn.call("initialize", &init_params, RPC_CALL)?;
        if init.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err("unexpected RPC protocol version".into());
        }
        {
            let caps = init
                .get("capabilities")
                .and_then(Value::as_array)
                .ok_or("initialize returned no capabilities")?;
            let names: Vec<&str> = caps.iter().filter_map(Value::as_str).collect();
            if names != ["sessions"] {
                return Err(
                    format!("negotiated capabilities {names:?}, expected [\"sessions\"]").into(),
                );
            }
        }
        {
            let client_id = init
                .get("clientId")
                .and_then(Value::as_str)
                .ok_or("initialize returned no client id")?;
            if !client_id.starts_with("dalgona-batteries-06#") {
                return Err(format!("unexpected RPC client id {client_id:?}").into());
            }
        }
        let opened_params = sonic_rs::json!({ "ref": { "type": "new", "workspace": workspace } });
        let opened = conn.call("session/open", &opened_params, RPC_CALL)?;
        let session = opened
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or("session/open returned no session id")?
            .to_owned();
        let subscribe_params = sonic_rs::json!({ "sessionId": session.as_str() });
        let subscribed = conn.call("session/subscribe", &subscribe_params, RPC_CALL)?;
        if subscribed.get("gen").and_then(Value::as_u64).is_none()
            || subscribed.get("seq").and_then(Value::as_u64).is_none()
        {
            return Err("session/subscribe returned no cursor".into());
        }
        let submit_params = sonic_rs::json!({
            "sessionId": session.as_str(),
            "command": {
                "type": "prompt",
                "expect": "idle",
                "content": [{ "type": "text", "text": "fetch the page" }],
            },
        });
        let submitted = conn.call("session/submit", &submit_params, RPC_CALL)?;
        if submitted.get("type").and_then(Value::as_str) != Some("accepted") {
            return Err("the prompt was not accepted".into());
        }
        let turn = submitted
            .get("turn")
            .and_then(Value::as_u64)
            .ok_or("the prompt returned no turn")?;
        Ok(Self {
            conn,
            session,
            turn,
            grant: None,
            unexpected_approval: None,
        })
    }

    fn answer(&mut self, request_id: &str, answer_type: &str) -> support::TestResult<()> {
        rpc_answer(&mut self.conn, &self.session, request_id, answer_type)
    }

    fn handle_grant(
        &mut self,
        request_id: String,
        question: Option<&Value>,
    ) -> support::TestResult<()> {
        let Some(question) = question else {
            self.answer(&request_id, "decline")?;
            return Ok(());
        };
        let ext = question.get("ext").and_then(Value::as_str).unwrap_or("");
        if ext != "web" || self.grant.is_some() {
            self.answer(&request_id, "decline")?;
            return Ok(());
        }
        let items = question.get("capabilities").and_then(Value::as_array);
        let Some(items) = items else {
            self.answer(&request_id, "decline")?;
            return Ok(());
        };
        if items.is_empty() || !items.iter().all(|item| item.as_str().is_some()) {
            self.answer(&request_id, "decline")?;
            return Ok(());
        }
        let observed: Vec<String> = items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
        let mut sorted = observed.clone();
        sorted.sort();
        if sorted != vec!["env".to_owned(), "net".to_owned()] {
            self.answer(&request_id, "decline")?;
            return Ok(());
        }
        let origin = question
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        self.answer(&request_id, "approve_for_session")?;
        self.grant = Some(ObservedGrant {
            request_id,
            ext: ext.to_owned(),
            origin,
            capabilities: observed,
        });
        Ok(())
    }

    fn handle_approval(
        &mut self,
        request_id: &str,
        question: Option<&Value>,
    ) -> support::TestResult<()> {
        // Independent contract: `FetchTool::classify` returns
        // `ToolClass::Read`, and `Policy::decide` maps `Read` via
        // `Gate::ReadOnly` to `Allow` in every mode (including `ask`), so a
        // tool `Approval` question for read-only `web_fetch` is unexpected.
        // Decline fail-closed and record it so `finish()` fails the test.
        let tool = question
            .and_then(|asked| asked.get("tool"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.answer(request_id, "decline")?;
        if self.unexpected_approval.is_none() {
            self.unexpected_approval = Some(format!("{request_id}:{tool}"));
        }
        Ok(())
    }

    fn handle_request_opened(
        &mut self,
        owner: Option<&str>,
        update: &Value,
    ) -> support::TestResult<()> {
        let request_id = update
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a request opened without an id")?
            .to_owned();
        if owner != Some(self.session.as_str()) {
            return Ok(());
        }
        if let Some(update_turn) = update.get("turn").and_then(Value::as_u64)
            && update_turn != self.turn
        {
            self.answer(&request_id, "decline")?;
            return Ok(());
        }
        let question = update.get("question");
        let qtype = question
            .and_then(|asked| asked.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if qtype == "grant" {
            self.handle_grant(request_id, question)?;
        } else if qtype == "approval" {
            self.handle_approval(&request_id, question)?;
        } else {
            self.answer(&request_id, "decline")?;
        }
        Ok(())
    }

    fn is_own_turn_end(&self, owner: Option<&str>, update: &Value) -> bool {
        owner == Some(self.session.as_str())
            && update.get("turn").and_then(Value::as_u64) == Some(self.turn)
    }

    fn pump(&mut self) -> support::TestResult<()> {
        let deadline = Instant::now() + RPC_TURN;
        'turn: loop {
            if Instant::now() >= deadline {
                return Err("timed out waiting for the live binary grant turn".into());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let window = remaining.min(Duration::from_secs(5));
            for (owner, update) in self.conn.drain(&["request_opened", "turn_ended"], window)? {
                let kind = update.get("type").and_then(Value::as_str).unwrap_or("");
                if kind == "request_opened" {
                    self.handle_request_opened(owner.as_deref(), &update)?;
                } else if kind == "turn_ended" && self.is_own_turn_end(owner.as_deref(), &update) {
                    break 'turn;
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> support::TestResult<GrantProof> {
        let Self {
            conn,
            grant,
            unexpected_approval,
            ..
        } = self;
        drop(conn);
        if let Some(unexpected) = unexpected_approval {
            return Err(format!(
                "unexpected tool Approval request opened ({unexpected}); read-only web_fetch must not ask"
            )
            .into());
        }
        let grant = grant.ok_or("the live binary Grant request never opened")?;
        Ok(GrantProof {
            request_id: grant.request_id,
            ext: grant.ext,
            origin: grant.origin,
            capabilities: grant.capabilities,
        })
    }
}

fn prepare_grant_scratch(public_url: &str) -> support::TestResult<support::Scratch> {
    let scratch = support::Scratch::new("binary-web-grant-pump")?;
    let args = format!(r#"{{"url":{}}}"#, sonic_rs::to_string(public_url)?);
    let fixture = scratch.path().join("scripted.jsonl");
    std::fs::write(
        &fixture,
        support::tool_step("fetch-1", "web_fetch", &args)
            + "\n"
            + &support::text_step("done")
            + "\n",
    )?;
    let config_home = scratch.path().join("xdg-config");
    let data_home = scratch.path().join("xdg-data");
    let cache_home = scratch.path().join("xdg-cache");
    let work = scratch.path().join("work");
    std::fs::create_dir_all(config_home.join("dalgona"))?;
    std::fs::create_dir_all(&data_home)?;
    std::fs::create_dir_all(&cache_home)?;
    std::fs::create_dir_all(&work)?;
    std::fs::write(
        config_home.join("dalgona/dal.toml"),
        format!(
            "approval = \"ask\"\nmodel = \"openai-responses/gpt-6\"\n{WEB_ONLY}\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    Ok(scratch)
}

fn connect_grant_rpc(scratch: &support::Scratch, proxy_url: &str) -> support::TestResult<RpcConn> {
    let (mut child, stdin, stdout) = support::spawn_dalgona_rpc(scratch, proxy_url)?;
    let start_deadline = Instant::now() + RPC_START;
    if let Some(status) = child.try_wait()? {
        return Err(format!("the rpc process exited early with {status}").into());
    }
    if Instant::now() >= start_deadline {
        return Err("the RPC child never became ready".into());
    }
    Ok(RpcConn::open(stdin, stdout, child))
}

fn read_grant_tool_text(scratch: &support::Scratch) -> support::TestResult<String> {
    let journal = support::session_journal(scratch)?;
    let results = support::journal_records(&journal, "tool_result");
    if results.len() != 1 {
        return Err(format!("expected one tool_result, found {}", results.len()).into());
    }
    tool_text(results[0])
}

/// Drives the positive fetch through the real `dalgona rpc` binary.
///
/// Isolated temp config with the offline scripted provider (`approval =
/// "ask"`, `WEB_ONLY`, fixture `fetch-1/web_fetch` then `done`) and the exact
/// loopback proxy env (`HTTP_PROXY`/`http_proxy`/`ALL_PROXY`/`all_proxy` =
/// proxy, `NO_PROXY`/`no_proxy` = empty). Same live session throughout:
/// answers ONLY the real service `grant` for extension `web` with exactly
/// `net`+`env` via `approve_for_session`; declines everything else, and any
/// unexpected tool `approval` question is declined fail-closed and fails via
/// `finish()`. Returns tool text plus
/// the recorded grant proof. All child state is owned by the connection
/// (`stdin`/`stdout` piped for JSONL, `stderr` to a temp log, `Drop` closes
/// stdin, drops the receiver, kills and waits the child to force EOF, then
/// joins the reader); every wait is bounded; no RPC actor leaks.
fn binary_grant_fetch_via_rpc(
    proxy: &LoopbackFixtureProxy,
    public_url: &str,
) -> support::TestResult<(String, GrantProof)> {
    let scratch = prepare_grant_scratch(public_url)?;
    let proxy_url = format!("http://{}", proxy.addr);
    let conn = connect_grant_rpc(&scratch, &proxy_url)?;
    let workspace = scratch.path().join("work").to_string_lossy().into_owned();
    let mut pump = GrantPump::open(conn, &workspace)?;
    pump.pump()?;
    let proof = pump.finish()?;
    let text = read_grant_tool_text(&scratch)?;
    Ok((text, proof))
}

fn fetch_through_proxy(
    proxy: &LoopbackFixtureProxy,
    label: &str,
    url: &str,
) -> support::TestResult<String> {
    let scratch = support::Scratch::new(label)?;
    let args = format!(r#"{{"url":{}}}"#, sonic_rs::to_string(url)?);
    let proxy_url = format!("http://{}", proxy.addr);
    let environment = [
        ("HTTP_PROXY", proxy_url.as_str()),
        ("http_proxy", proxy_url.as_str()),
        ("ALL_PROXY", proxy_url.as_str()),
        ("all_proxy", proxy_url.as_str()),
        ("NO_PROXY", ""),
        ("no_proxy", ""),
    ];
    let journal = support::run_scripted_print_with_env(
        &scratch,
        WEB_ONLY,
        &[
            support::tool_step("fetch-1", "web_fetch", &args),
            support::text_step("done"),
        ],
        "fetch the page",
        &environment,
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 1, "{journal}");
    tool_text(results[0])
}

fn fetch_without_proxy(label: &str, url: &str) -> support::TestResult<String> {
    let scratch = support::Scratch::new(label)?;
    let args = format!(r#"{{"url":{}}}"#, sonic_rs::to_string(url)?);
    let journal = support::run_scripted_print(
        &scratch,
        WEB_ONLY,
        &[
            support::tool_step("fetch-1", "web_fetch", &args),
            support::text_step("done"),
        ],
        "fetch the page",
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 1, "{journal}");
    tool_text(results[0])
}

#[test]
fn web_fetch_converts_local_html_to_markdown() -> support::TestResult<()> {
    let registration_scratch = support::Scratch::new("web-registration")?;
    let product = support::build_product(registration_scratch.path().to_path_buf(), None)?;
    let web = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "web")
        .ok_or("the product has no web extension")?;
    assert!(
        web.tools()
            .iter()
            .any(|(tool, _)| tool.name().as_str() == "web_fetch"),
        "the registered web extension publishes web_fetch"
    );
    drop(product);

    support::dalgona_binary()?;
    let proxy = LoopbackFixtureProxy::spawn()?;
    let public_url = format!("http://{PUBLIC_FIXTURE_HOST}:{}/page", proxy.addr.port());
    let (text, proof) = binary_grant_fetch_via_rpc(&proxy, &public_url)?;
    assert_eq!(proof.ext, "web");
    assert_eq!(
        {
            let mut caps = proof.capabilities.clone();
            caps.sort();
            caps
        },
        vec!["env".to_owned(), "net".to_owned()]
    );
    assert!(
        !proof.request_id.is_empty(),
        "service-Grant request identity is recorded: {proof:?}"
    );
    // `finish()` already failed the test if any unexpected tool `Approval`
    // question opened, so reaching here proves zero tool Approval requests.
    let request = proxy.finish()?.ok_or_else(|| {
        format!("the public web_fetch call never reached its loopback proxy; result: {text}")
    })?;
    assert_eq!(request.method, "GET");
    assert_eq!(request.target, public_url);
    let result: Value = sonic_rs::from_str(&text)?;
    assert_eq!(
        result.get("url").and_then(Value::as_str),
        Some(public_url.as_str())
    );
    assert_eq!(
        result.get("final_url").and_then(Value::as_str),
        Some(public_url.as_str())
    );
    assert_eq!(result.get("status").and_then(Value::as_u64), Some(200));
    assert_eq!(
        result.get("content_type").and_then(Value::as_str),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(
        result.get("markdown").and_then(Value::as_str),
        Some("# Hello")
    );
    assert_eq!(
        result.get("truncated").and_then(Value::as_bool),
        Some(false)
    );
    let expected_binary_bytes =
        u64::try_from(FIXTURE_BODY.len()).expect("fixture body length fits in u64");
    assert_eq!(
        result.get("raw_bytes").and_then(Value::as_u64),
        Some(expected_binary_bytes)
    );
    Ok(())
}

#[test]
fn web_fetch_refuses_private_targets_and_malformed_urls() -> support::TestResult<()> {
    support::dalgona_binary()?;
    let proxy = LoopbackFixtureProxy::spawn()?;
    let loopback =
        fetch_through_proxy(&proxy, "web-loopback-refused", "http://127.0.0.1:9/private")?;
    assert!(
        loopback.contains("127.0.0.1 is a private address; fetch public URLs only"),
        "loopback is refused before fetching: {loopback}"
    );
    assert_eq!(
        proxy.finish()?,
        None,
        "a private URL must not contact the loopback transport"
    );

    let scheme = fetch_without_proxy("web-bad-scheme", "ftp://example.com/gate")?;
    assert!(
        scheme.contains("unsupported URL scheme"),
        "unsupported schemes are refused: {scheme}"
    );
    let invalid = fetch_without_proxy("web-invalid-url", "::: not a url :::")?;
    assert!(
        invalid.contains("invalid URL:"),
        "malformed URLs are refused: {invalid}"
    );
    Ok(())
}
