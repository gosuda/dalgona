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
    sync::{Mutex, mpsc},
    time::timeout,
};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

use crate::mcp::{
    Budgets, McpError, STDERR_RING, TransportError,
    tools::{Key, ServerDecl, resolve_executable},
};

const LINE_MAX: usize = 1_048_576;
const PENDING_MAX: usize = 64;
const EVENT_CAPACITY: usize = 8;
const POST_KILL_WAIT: Duration = Duration::from_secs(1);

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<RawJson, McpError>>>>>;

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
    #[expect(
        dead_code,
        reason = "crash-diagnostics ring read by stderr_excerpt on server failure paths"
    )]
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    reader_task: AbortOnDropHandle<()>,
    stderr_task: AbortOnDropHandle<()>,
    cancel: CancellationToken,
}

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
        let program = command.first().ok_or_else(|| McpError::Start {
            key: key.display(),
            cause: "stdio command has no argv[0]".to_owned(),
        })?;
        let path = environment.path.clone();
        let lookup_program = program.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            resolve_executable(&lookup_program, path.as_deref())
        })
        .await
        .map_err(|error| McpError::Start {
            key: key.display(),
            cause: format!("command lookup failed: {error}"),
        })?
        .ok_or_else(|| McpError::Start {
            key: key.display(),
            cause: format!("command {program:?} was not found on PATH"),
        })?;

        let mut child = Self::spawn_piped(
            key.clone(),
            resolved,
            command,
            env,
            environment,
            budgets,
        )
        .await?;
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
            key.clone(),
            cancel.clone(),
        )));
        #[expect(
            clippy::disallowed_methods,
            reason = "stdio server owns the abort-on-drop stderr drain"
        )]
        let stderr_task =
            AbortOnDropHandle::new(tokio::spawn(read_stderr(stderr, Arc::clone(&stderr_tail))));

        Ok(Self {
            key,
            child,
            stdin,
            pending,
            stderr_tail,
            reader_task,
            stderr_task,
            cancel,
        })
    }

    async fn spawn_piped(
        key: Key,
        resolved: std::path::PathBuf,
        command: &[Box<str>],
        env: &std::collections::BTreeMap<Box<str>, Box<str>>,
        environment: &ProcessEnvironment,
        budgets: &Budgets,
    ) -> Result<Box<dyn ChildWrapper>, McpError> {
        let program_args = command
            .iter()
            .skip(1)
            .map(|arg| OsString::from(arg.as_ref()))
            .collect::<Vec<_>>();
        let declarations = env
            .iter()
            .map(|(key, value)| (OsString::from(key.as_ref()), OsString::from(value.as_ref())))
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
                return Err(McpError::Exited {
                    key: self.key.display(),
                    code: process_status(&self.child).await,
                });
            };
            if stdin.write_all(body.as_str().as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
            {
                return Err(McpError::Exited {
                    key: self.key.display(),
                    code: process_status(&self.child).await,
                });
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
    #[expect(
        dead_code,
        reason = "crash-diagnostics reader for MCP server failure paths, wired with error reporting"
    )]
    pub(crate) async fn stderr_excerpt(&self) -> String {
        let mut tail = self.stderr_tail.lock().await;
        String::from_utf8_lossy(tail.make_contiguous()).into_owned()
    }

    /// Closes stdin, waits for the grace period, then kills and reaps the process tree.
    pub(crate) async fn shutdown(&self, grace: Duration) -> Result<(), McpError> {
        clear_pending(&self.pending).await;
        self.cancel.cancel();
        self.stdin.lock().await.take();

        let mut child = self.child.lock().await;
        let result = match timeout(grace, child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(McpError::Exited {
                key: self.key.display(),
                code: -1,
            }),
            Err(_) => match child.start_kill() {
                Ok(()) => match timeout(POST_KILL_WAIT, child.wait()).await {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(_)) | Err(_) => Err(McpError::Exited {
                        key: self.key.display(),
                        code: -1,
                    }),
                },
                Err(_) => Err(McpError::Exited {
                    key: self.key.display(),
                    code: -1,
                }),
            },
        };
        drop(child);
        self.reader_task.abort();
        self.stderr_task.abort();
        result
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
                return Err(TransportError::Mcp(McpError::Exited {
                    key: self.key.display(),
                    code: process_status(&self.child).await,
                }));
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

async fn process_status(child: &Arc<Mutex<Box<dyn ChildWrapper>>>) -> i32 {
    let mut child = child.lock().await;
    child.try_wait().ok().flatten().map_or(-1, exit_code)
}

async fn read_stdout(
    stdout: ChildStdout,
    child: Arc<Mutex<Box<dyn ChildWrapper>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Pending,
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
                fail_pending(
                    &pending,
                    McpError::Exited {
                        key: key.display(),
                        code: process_status(&child).await,
                    },
                )
                .await;
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
                fail_pending(
                    &pending,
                    McpError::Exited {
                        key: key.display(),
                        code: -1,
                    },
                )
                .await;
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

async fn deliver(
    pending: &Pending,
    id: u64,
    result: Result<RawJson, McpError>,
) {
    let sender = pending.lock().await.get(&id).cloned();
    if let Some(sender) = sender {
        let _ = sender.send(result).await;
    }
}

async fn fail_pending(
    pending: &Pending,
    error: McpError,
) {
    let mut pending = pending.lock().await;
    let senders = pending.values().cloned().collect::<Vec<_>>();
    pending.clear();
    drop(pending);
    for sender in senders {
        let _ = sender.send(Err(error.clone())).await;
    }
}

async fn clear_pending(
    pending: &Pending,
) {
    pending.lock().await.clear();
}

async fn read_stderr(mut stderr: ChildStderr, tail: Arc<Mutex<VecDeque<u8>>>) {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
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
}
