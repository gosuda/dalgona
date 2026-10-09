// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::{
    collections::{HashMap, VecDeque},
    ffi::{OsStr, OsString},
    io,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use dal_core::RawJson;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use sonic_rs::JsonValueTrait;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Mutex, mpsc, watch},
    time::timeout,
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

use crate::mcp::{
    Budgets, McpError, STDERR_RING, TransportError, stderr_diagnostic,
    tools::{Key, ServerDecl, resolve_executable},
};

const LINE_MAX: usize = 1_048_576;
const PENDING_MAX: usize = 64;
const EVENT_CAPACITY: usize = 8;
const POST_KILL_WAIT: Duration = Duration::from_secs(1);

/// The process environment allowlist captured through the host's env service.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessEnvironment {
    pub(crate) path: Option<OsString>,
    pub(crate) home: Option<OsString>,
    pub(crate) tmpdir: Option<OsString>,
}

/// A started stdio server with one response demultiplexer and bounded diagnostics.
pub(crate) struct StdioTransport {
    key: Key,
    child: Arc<Mutex<Box<dyn ChildWrapper>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Pending,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    stderr_done: watch::Receiver<bool>,
    reader_task: AbortOnDropHandle<()>,
    stderr_task: AbortOnDropHandle<()>,
    cancel: CancellationToken,
}

/// Reply channels for in-flight requests, keyed by wire id.
type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<RawJson, McpError>>>>>;

impl StdioTransport {
    /// Spawns a declared command without a shell and with only the captured env allowlist.
    pub(crate) async fn start(
        key: Key,
        declaration: &ServerDecl,
        environment: &ProcessEnvironment,
        budgets: &Budgets,
    ) -> Result<Self, McpError> {
        let ServerDecl::Stdio { command, env } = declaration else {
            return Err(McpError::Start {
                key: key.display(),
                cause: "stdio transport received an HTTP declaration".to_owned(),
            });
        };
        let resolved = resolve_program(&key, command, environment).await?;
        let mut child = spawn_child(&key, resolved, command, env, environment, budgets).await?;
        let stdin = child.stdin().take();
        let stdout = child.stdout().take();
        let stderr = child.stderr().take();
        let (Some(stdin), Some(stdout), Some(stderr)) = (stdin, stdout, stderr) else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(McpError::Start {
                key: key.display(),
                cause: "process wrapper did not provide piped stdio".to_owned(),
            });
        };

        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_RING)));
        let (stderr_done_tx, stderr_done_rx) = watch::channel(false);
        let cancel = CancellationToken::new();
        #[expect(
            clippy::disallowed_methods,
            reason = "stdio server owns the abort-on-drop reader task"
        )]
        let reader_task = AbortOnDropHandle::new(tokio::spawn(read_stdout(
            stdout,
            Arc::clone(&child),
            Arc::clone(&stdin),
            Arc::clone(&pending),
            Arc::clone(&stderr_tail),
            stderr_done_rx.clone(),
            key.clone(),
            cancel.clone(),
        )));
        #[expect(
            clippy::disallowed_methods,
            reason = "stdio server owns the abort-on-drop stderr drain"
        )]
        let stderr_task = AbortOnDropHandle::new(tokio::spawn(read_stderr(
            stderr,
            Arc::clone(&stderr_tail),
            stderr_done_tx,
        )));

        Ok(Self {
            key,
            child,
            stdin,
            pending,
            stderr_tail,
            stderr_done: stderr_done_rx,
            reader_task,
            stderr_task,
            cancel,
        })
    }

    /// Writes one complete JSON-RPC line and returns its correlated event stream.
    ///
    /// Cancellation never becomes a public [`McpError`] string; it returns
    /// [`TransportError::Cancelled`] so the session client maps it to the
    /// host `ServiceError::Cancelled`.
    pub(crate) async fn send(
        &self,
        id: u64,
        body: &RawJson,
        cancel: &CancellationToken,
    ) -> Result<mpsc::Receiver<Result<RawJson, McpError>>, TransportError> {
        if cancel.is_cancelled() || self.cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if body.as_str().len() > LINE_MAX {
            return Err(TransportError::Mcp(McpError::Protocol {
                code: -32600,
                message: "MCP request exceeds the stdio line limit".to_owned(),
            }));
        }
        let (sender, receiver) = mpsc::channel(EVENT_CAPACITY);
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= PENDING_MAX {
                return Err(TransportError::Mcp(McpError::Protocol {
                    code: -32000,
                    message: "too many concurrent MCP requests".to_owned(),
                }));
            }
            if pending.contains_key(&id) {
                return Err(TransportError::Mcp(McpError::Protocol {
                    code: -32600,
                    message: "MCP request id is already in flight".to_owned(),
                }));
            }
            pending.insert(id, sender);
        }

        let write = async {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                let code = wait_child(&self.child, cancel).await;
                return Err(self.exited_error(code).await);
            };
            if stdin.write_all(body.as_str().as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
            {
                let code = wait_child(&self.child, cancel).await;
                return Err(self.exited_error(code).await);
            }
            Ok(())
        };
        let write_result = tokio::select! {
            () = cancel.cancelled() => Err(TransportError::Cancelled),
            () = self.cancel.cancelled() => Err(TransportError::Cancelled),
            result = async { write.await.map_err(TransportError::Mcp) } => result,
        };
        if let Err(error) = write_result {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        Ok(receiver)
    }

    /// Removes a completed or cancelled request from the response table.
    pub(crate) async fn finish(&self, id: u64) {
        self.pending.lock().await.remove(&id);
    }

    /// Returns the bounded stderr excerpt retained for crash diagnostics.
    pub(crate) async fn stderr_excerpt(&self) -> String {
        read_stderr_excerpt(&self.stderr_tail).await
    }
    async fn exited_error(&self, code: i32) -> McpError {
        let mut stderr_done = self.stderr_done.clone();
        wait_for_stderr(&mut stderr_done).await;
        let excerpt = self.stderr_excerpt().await;
        McpError::Exited {
            key: self.key.display(),
            code,
            diagnostic: stderr_diagnostic(&excerpt),
        }
    }

    /// Closes stdin, waits for the grace period, then kills and reaps the process tree.
    pub(crate) async fn shutdown(&self, grace: Duration) -> Result<(), McpError> {
        clear_pending(&self.pending).await;
        self.cancel.cancel();
        self.stdin.lock().await.take();

        let mut child = self.child.lock().await;
        let outcome = match timeout(grace, child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(-1),
            Err(_) => match child.start_kill() {
                Ok(()) => match timeout(POST_KILL_WAIT, child.wait()).await {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(_)) | Err(_) => Err(-1),
                },
                Err(_) => Err(-1),
            },
        };
        drop(child);
        self.reader_task.abort();
        self.stderr_task.abort();
        match outcome {
            Ok(()) => Ok(()),
            Err(code) => Err(self.exited_error(code).await),
        }
    }

    /// Sends an MCP notification without allocating a response slot.
    pub(crate) async fn notify(
        &self,
        body: &RawJson,
        cancel: &CancellationToken,
    ) -> Result<(), TransportError> {
        let write = async {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                let code = wait_child(&self.child, cancel).await;
                return Err(TransportError::Mcp(self.exited_error(code).await));
            };
            stdin
                .write_all(body.as_str().as_bytes())
                .await
                .map_err(|error| {
                    TransportError::Mcp(McpError::Start {
                        key: self.key.display(),
                        cause: error.to_string(),
                    })
                })?;
            stdin.write_all(b"\n").await.map_err(|error| {
                TransportError::Mcp(McpError::Start {
                    key: self.key.display(),
                    cause: error.to_string(),
                })
            })
        };
        tokio::select! {
            () = cancel.cancelled() => Err(TransportError::Cancelled),
            () = self.cancel.cancelled() => Err(TransportError::Cancelled),
            result = write => result,
        }
    }

    /// Notifies the server that one correlated request no longer has a waiting caller.
    pub(crate) async fn cancel_request(&self, id: u64, _version: &str, cancel: &CancellationToken) {
        let body = format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/cancelled","params":{{"requestId":{id}}}}}"#
        );
        if let Ok(body) = RawJson::parse(&body) {
            let _ = self.notify(&body, cancel).await;
        }
    }
}

fn set_env(command: &mut Command, name: &str, value: Option<&OsStr>) {
    if let Some(value) = value {
        command.env(name, value);
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map_or(-1, |signal| 128 + signal)
    }
    #[cfg(not(unix))]
    {
        -1
    }
}

async fn wait_child(child: &Arc<Mutex<Box<dyn ChildWrapper>>>, cancel: &CancellationToken) -> i32 {
    let mut child = child.lock().await;
    tokio::select! {
        () = cancel.cancelled() => -1,
        result = timeout(POST_KILL_WAIT, child.wait()) => match result {
            Ok(Ok(status)) => exit_code(status),
            Ok(Err(_)) | Err(_) => -1,
        },
    }
}
async fn read_stderr_excerpt(tail: &Arc<Mutex<VecDeque<u8>>>) -> String {
    let mut tail = tail.lock().await;
    String::from_utf8_lossy(tail.make_contiguous()).into_owned()
}

async fn wait_for_stderr(done: &mut watch::Receiver<bool>) {
    if *done.borrow() {
        return;
    }
    let _ = timeout(POST_KILL_WAIT, async {
        while done.changed().await.is_ok() && !*done.borrow() {}
    })
    .await;
}

async fn crash_error(
    child: &Arc<Mutex<Box<dyn ChildWrapper>>>,
    stderr_tail: &Arc<Mutex<VecDeque<u8>>>,
    stderr_done: &mut watch::Receiver<bool>,
    key: &Key,
    cancel: &CancellationToken,
) -> McpError {
    let code = wait_child(child, cancel).await;
    wait_for_stderr(stderr_done).await;
    let excerpt = read_stderr_excerpt(stderr_tail).await;
    McpError::Exited {
        key: key.display(),
        code,
        diagnostic: stderr_diagnostic(&excerpt),
    }
}

/// Resolves the declared argv[0] on the captured PATH snapshot.
async fn resolve_program(
    key: &Key,
    command: &[Box<str>],
    environment: &ProcessEnvironment,
) -> Result<std::path::PathBuf, McpError> {
    let program = command.first().ok_or_else(|| McpError::Start {
        key: key.display(),
        cause: "stdio command has no argv[0]".to_owned(),
    })?;
    let path = environment.path.clone();
    let lookup_program = program.clone();
    tokio::task::spawn_blocking(move || resolve_executable(&lookup_program, path.as_deref()))
        .await
        .map_err(|error| McpError::Start {
            key: key.display(),
            cause: format!("command lookup failed: {error}"),
        })?
        .ok_or_else(|| McpError::Start {
            key: key.display(),
            cause: format!("command {program:?} was not found on PATH"),
        })
}

/// Spawns the wrapped process with cleared env, declared vars, and kill ownership.
async fn spawn_child(
    key: &Key,
    resolved: std::path::PathBuf,
    command: &[Box<str>],
    env: &std::collections::BTreeMap<Box<str>, Box<str>>,
    environment: &ProcessEnvironment,
    budgets: &Budgets,
) -> Result<Box<dyn ChildWrapper>, McpError> {
    let program_args = command
        .iter()
        .skip(1)
        .map(|arg| OsString::from(&**arg))
        .collect::<Vec<_>>();
    let declarations = env
        .iter()
        .map(|(key, value)| (OsString::from(&**key), OsString::from(&**value)))
        .collect::<Vec<_>>();
    let path = environment.path.clone();
    let home = environment.home.clone();
    let tmpdir = environment.tmpdir.clone();
    let mut wrapped = CommandWrap::with_new(resolved, move |command: &mut Command| {
        command
            .args(program_args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        set_env(command, "PATH", path.as_deref());
        set_env(command, "HOME", home.as_deref());
        set_env(command, "TMPDIR", tmpdir.as_deref());
        command.envs(declarations);
    });
    wrapped.wrap(KillOnDrop);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);

    let mut spawn_task = tokio::task::spawn_blocking(move || wrapped.spawn());
    let Ok(spawn_result) = timeout(budgets.start, &mut spawn_task).await else {
        if let Ok(Ok(mut child)) = spawn_task.await {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        return Err(McpError::Start {
            key: key.display(),
            cause: format!("timed out after {} s", budgets.start.as_secs()),
        });
    };
    spawn_result
        .map_err(|error| McpError::Start {
            key: key.display(),
            cause: format!("process spawn task failed: {error}"),
        })?
        .map_err(|error| McpError::Start {
            key: key.display(),
            cause: error.to_string(),
        })
}

#[expect(
    clippy::too_many_arguments,
    reason = "stdio reader receives the process-owned channels and cancellation token"
)]
async fn read_stdout(
    stdout: ChildStdout,
    child: Arc<Mutex<Box<dyn ChildWrapper>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Pending,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    mut stderr_done: watch::Receiver<bool>,
    key: Key,
    cancel: CancellationToken,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let line = tokio::select! {
            () = cancel.cancelled() => return,
            result = read_protocol_line(&mut reader) => result,
        };
        let line = match line {
            Ok(Some(line)) => line,
            Ok(None) => {
                let error =
                    crash_error(&child, &stderr_tail, &mut stderr_done, &key, &cancel).await;
                fail_pending(&pending, error).await;
                return;
            }
            Err(_) => {
                fail_pending(&pending, McpError::InvalidLine { key: key.display() }).await;
                return;
            }
        };
        let Ok(text) = std::str::from_utf8(&line) else {
            fail_pending(&pending, McpError::InvalidLine { key: key.display() }).await;
            return;
        };
        let Ok(message) = RawJson::parse(text) else {
            fail_pending(&pending, McpError::InvalidLine { key: key.display() }).await;
            return;
        };
        let Ok(value) = message.decode_as::<sonic_rs::Value>() else {
            fail_pending(&pending, McpError::InvalidLine { key: key.display() }).await;
            return;
        };
        let id_value = value.get("id");
        let method = value
            .get("method")
            .and_then(sonic_rs::JsonValueTrait::as_str);
        if id_value.is_some() && method.is_some() {
            if method == Some("elicitation/create") {
                let token = value
                    .get("params")
                    .and_then(|params| params.get("_meta"))
                    .and_then(|meta| meta.get("progressToken"))
                    .and_then(sonic_rs::JsonValueTrait::as_str);
                if let Some(id) = token.and_then(parse_progress_id) {
                    deliver(&pending, id, Ok(message)).await;
                    continue;
                }
            }
            let response = match unsupported_request(&value) {
                Ok(response) => response,
                Err(error) => {
                    fail_pending(&pending, error).await;
                    return;
                }
            };
            let mut stdin = stdin.lock().await;
            if let Some(stdin) = stdin.as_mut()
                && (stdin.write_all(response.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err())
            {
                let error =
                    crash_error(&child, &stderr_tail, &mut stderr_done, &key, &cancel).await;
                fail_pending(&pending, error).await;
                return;
            }
            continue;
        }
        if let Some(id) = id_value.and_then(sonic_rs::JsonValueTrait::as_u64) {
            deliver(&pending, id, Ok(message)).await;
            continue;
        }
        if method == Some("notifications/progress") {
            let token = value
                .get("params")
                .and_then(|params| params.get("_meta"))
                .and_then(|meta| meta.get("progressToken"))
                .and_then(sonic_rs::JsonValueTrait::as_str);
            if let Some(id) = token.and_then(parse_progress_id) {
                deliver(&pending, id, Ok(message)).await;
            }
        }
    }
}

async fn read_protocol_line<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unterminated JSON-RPC stdout line",
            ));
        }
        let Some(index) = available.iter().position(|byte| *byte == b'\n') else {
            let count = available.len();
            if line.len().saturating_add(count) > LINE_MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "JSON-RPC stdout line exceeds the limit",
                ));
            }
            line.extend_from_slice(available);
            reader.consume(count);
            continue;
        };
        let count = index + 1;
        if line.len().saturating_add(count) > LINE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON-RPC stdout line exceeds the limit",
            ));
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        return Ok(Some(line));
    }
}

fn parse_progress_id(token: &str) -> Option<u64> {
    token.strip_prefix("t-")?.parse().ok()
}

fn unsupported_request(value: &sonic_rs::Value) -> Result<String, McpError> {
    let id = value.get("id").ok_or_else(|| McpError::Protocol {
        code: -32600,
        message: "server request has no id".to_owned(),
    })?;
    let id = sonic_rs::to_string(id).map_err(|error| McpError::Protocol {
        code: -32600,
        message: format!("invalid server request id: {error}"),
    })?;
    let method = value
        .get("method")
        .and_then(sonic_rs::JsonValueTrait::as_str)
        .unwrap_or("unknown");
    let message = match method {
        "sampling/createMessage" => "client does not support sampling",
        "roots/list" => "client does not support roots",
        _ => "client does not support server-initiated requests",
    };
    let message = sonic_rs::to_string(message).map_err(|error| McpError::Protocol {
        code: -32600,
        message: format!("could not encode server request error: {error}"),
    })?;
    Ok(format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"error\":{{\"code\":-32601,\"message\":{message}}}}}"
    ))
}

async fn deliver(pending: &Pending, id: u64, result: Result<RawJson, McpError>) {
    let sender = pending.lock().await.get(&id).cloned();
    if let Some(sender) = sender {
        let _ = sender.send(result).await;
    }
}

async fn fail_pending(pending: &Pending, error: McpError) {
    let mut pending = pending.lock().await;
    let senders = pending.values().cloned().collect::<Vec<_>>();
    pending.clear();
    drop(pending);
    for sender in senders {
        let _ = sender.send(Err(error.clone())).await;
    }
}

async fn clear_pending(pending: &Pending) {
    pending.lock().await.clear();
}

async fn read_stderr(
    mut stderr: ChildStderr,
    tail: Arc<Mutex<VecDeque<u8>>>,
    done: watch::Sender<bool>,
) {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let mut tail = tail.lock().await;
        if count >= STDERR_RING {
            tail.clear();
            tail.extend(&buffer[count - STDERR_RING..count]);
            continue;
        }
        let excess = tail.len().saturating_add(count).saturating_sub(STDERR_RING);
        tail.drain(..excess);
        tail.extend(&buffer[..count]);
    }
    let _ = done.send(true);
}
