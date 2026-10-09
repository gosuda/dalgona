#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs real dalgon processes"
)]
//! Reattaches the real TUI after its server-side WebSocket connection drops.

#[expect(
    dead_code,
    reason = "PTY support helpers are shared across TUI gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    fmt::Write as _,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command, dalgon_command_with_fixture};
use sonic_rs::{JsonValueTrait, Value};
use support::TestDir;

#[test]
fn remote_tui_reattaches_after_dropped_websocket() -> Result<(), Box<dyn Error + Send + Sync>> {
    let server_home = TestDir::new()?;
    let client_home = TestDir::new()?;
    let marker = client_home.path().join("remote-exec-started");
    let exec = format!("touch {} && exec sleep 5", shell_quote(&marker));
    let fixture = exec_then_reply_fixture(&exec, "remote session continued after socket drop")?;

    let mut server_command = dalgon_command_with_fixture(server_home.path(), &fixture)?;
    let port = free_port()?;
    let port_text = port.to_string();
    let serve_log = server_home.path().join("serve.stderr");
    server_command
        .args(["serve", "--bind", "127.0.0.1", "--port", &port_text])
        .stdout(Stdio::null())
        .stderr(Stdio::from(std::fs::File::create(&serve_log)?));
    let mut server = ServeChild::spawn(server_command.spawn()?);
    wait_for_listener(port, Duration::from_secs(10))?;

    let proxy = TcpProxy::start(SocketAddr::from(([127, 0, 0, 1], port)))?;
    let mut client_command = dalgon_command(client_home.path(), &["client provider is bypassed"])?;
    let connect_addr = format!("ws://{}/v1/ws", proxy.address());
    client_command.args([
        "--screen",
        "inline",
        "--approval",
        "ask",
        "--connect",
        &connect_addr,
    ]);
    let mut terminal = PtyProcess::spawn(&mut client_command, 100, 30)?;
    if let Err(error) = terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    ) {
        let log = std::fs::read_to_string(&serve_log).unwrap_or_default();
        return Err(format!("{error}\nserve stderr:\n{log}").into());
    }
    terminal.collect_for(Duration::from_millis(5))?;
    terminal.write(b"start the remote turn\r")?;
    terminal.wait_for(
        dal_tui::copy::ids::DIALOG_ACTIONS_SHORT.as_bytes(),
        Duration::from_secs(15),
    )?;
    terminal.write(b"y")?;
    wait_for_path(&marker, Duration::from_secs(10))?;
    terminal.wait_for(
        dal_tui::copy::ids::STATE_WORKING.as_bytes(),
        Duration::from_secs(10),
    )?;
    let last_observed_before_drop = proxy
        .server_update_cursor(0)
        .expect("the active session sent sequenced updates before the socket drop");
    proxy.drop_connection()?;
    proxy.wait_for_connections(2, Duration::from_secs(15))?;
    terminal.wait_for(
        b"remote session continued after socket drop",
        Duration::from_secs(15),
    )?;
    terminal.wait_for_count(b"enter send", 2, Duration::from_secs(15))?;
    let cursors = proxy.subscription_cursors();
    assert!(
        cursors.len() >= 2,
        "the remote TUI must resubscribe after the socket drop"
    );
    let first_cursor = cursors.first().unwrap();
    let resumed_cursor = cursors.last().unwrap();
    assert_eq!(resumed_cursor.0, first_cursor.0);
    assert!(resumed_cursor.1 > 0 && resumed_cursor.1 <= last_observed_before_drop.1);
    assert_eq!(resumed_cursor.0, last_observed_before_drop.0);

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(10))?;
    assert!(status.success(), "remote TUI exited with {status}");
    assert!(
        server
            .is_running()
            .expect("serve process status is observable")
    );
    let _first_output_byte = terminal.output().first().unwrap();
    Ok(())
}

fn exec_then_reply_fixture(
    command: &str,
    response: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let command = sonic_rs::to_string(command)?;
    let response = sonic_rs::to_string(response)?;
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"call-exec\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-exec\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{command},\"timeout_seconds\":30}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{response}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    ))
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn wait_for_listener(port: u16, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(());
            }
            Err(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
}

fn wait_for_path(path: &std::path::Path, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} was not created by the remote turn", path.display()),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

struct ServeChild {
    child: Child,
}

impl ServeChild {
    fn spawn(child: Child) -> Self {
        Self { child }
    }

    fn is_running(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }
}

impl Drop for ServeChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct TcpProxy {
    address: SocketAddr,
    drop_sender: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    client_frames: Arc<Mutex<Vec<Arc<Mutex<FrameCapture>>>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TcpProxy {
    fn start(target: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (drop_sender, drop_receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let connections = Arc::new(AtomicUsize::new(0));
        let thread_connections = Arc::clone(&connections);
        let client_frames = Arc::new(Mutex::new(Vec::new()));
        let thread_frames = Arc::clone(&client_frames);
        let thread = thread::spawn(move || {
            proxy_loop(
                listener,
                target,
                drop_receiver,
                thread_stop,
                thread_connections,
                thread_frames,
            );
        });
        Ok(Self {
            address,
            drop_sender,
            stop,
            connections,
            client_frames,
            thread: Some(thread),
        })
    }

    fn address(&self) -> SocketAddr {
        self.address
    }

    fn drop_connection(&self) -> io::Result<()> {
        self.drop_sender
            .send(())
            .map_err(|error| io::Error::new(io::ErrorKind::BrokenPipe, error))
    }

    fn wait_for_connections(&self, count: usize, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        while self.connections.load(Ordering::Acquire) < count {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("remote TUI did not make connection {count}"),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    fn subscription_cursors(&self) -> Vec<(u64, u64)> {
        let captures = self
            .client_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        captures
            .iter()
            .flat_map(|capture| {
                let capture = capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                capture
                    .client
                    .messages
                    .iter()
                    .filter_map(|message| subscription_cursor(message))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn server_update_cursor(&self, connection: usize) -> Option<(u64, u64)> {
        let captures = self
            .client_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(capture) = captures.get(connection)?.lock() else {
            return None;
        };
        capture
            .server
            .messages
            .iter()
            .filter_map(|message| update_cursor(message))
            .next_back()
    }
}

impl Drop for TcpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "arguments are moved into the spawned proxy thread"
)]
fn proxy_loop(
    listener: TcpListener,
    target: SocketAddr,
    drop_receiver: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    client_frames: Arc<Mutex<Vec<Arc<Mutex<FrameCapture>>>>>,
) {
    while !stop.load(Ordering::Acquire) {
        let (client, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(_) => return,
        };
        // BSD accepts inherit the listener's O_NONBLOCK; the relays block on
        // reads and must not see the flag.
        if client.set_nonblocking(false).is_err() {
            continue;
        }
        let Ok(upstream) = TcpStream::connect(target) else {
            continue;
        };
        connections.fetch_add(1, Ordering::AcqRel);
        let capture = Arc::new(Mutex::new(FrameCapture::default()));
        client_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(&capture));
        let (finished_sender, finished_receiver) = mpsc::channel();
        let client_to_upstream = client.try_clone();
        let upstream_to_client = upstream.try_clone();
        let (Ok(client_reader), Ok(upstream_reader)) = (client_to_upstream, upstream_to_client)
        else {
            let _ = client.shutdown(Shutdown::Both);
            let _ = upstream.shutdown(Shutdown::Both);
            continue;
        };
        let upstream_writer = upstream.try_clone();
        let client_writer = client.try_clone();
        let (Ok(upstream_writer), Ok(client_writer)) = (upstream_writer, client_writer) else {
            let _ = client.shutdown(Shutdown::Both);
            let _ = upstream.shutdown(Shutdown::Both);
            continue;
        };
        let first_finished = finished_sender.clone();
        let client_capture = Arc::clone(&capture);
        let server_capture = Arc::clone(&capture);
        let serve_port = target.port();
        let client_to_upstream = thread::spawn(move || {
            forward(
                client_reader,
                upstream_writer,
                Some((client_capture, TraceDirection::ClientToServer)),
                Some(serve_port),
            );
            let _ = first_finished.send(());
        });
        let upstream_to_client = thread::spawn(move || {
            forward(
                upstream_reader,
                client_writer,
                Some((server_capture, TraceDirection::ServerToClient)),
                None,
            );
            let _ = finished_sender.send(());
        });
        loop {
            if stop.load(Ordering::Acquire)
                || drop_receiver.try_recv().is_ok()
                || finished_receiver.try_recv().is_ok()
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let _ = client.shutdown(Shutdown::Both);
        let _ = upstream.shutdown(Shutdown::Both);
        let _ = client_to_upstream.join();
        let _ = upstream_to_client.join();
    }
}

#[derive(Default)]
struct FrameCapture {
    client: WebSocketStream,
    server: WebSocketStream,
}

impl FrameCapture {
    fn push_client(&mut self, bytes: &[u8]) {
        self.client.push(bytes);
    }

    fn push_server(&mut self, bytes: &[u8]) {
        self.server.push(bytes);
    }
}

#[derive(Default)]
struct WebSocketStream {
    bytes: Vec<u8>,
    frame_start: Option<usize>,
    messages: Vec<String>,
}

impl WebSocketStream {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        self.parse_available();
    }

    fn parse_available(&mut self) {
        if self.frame_start.is_none() {
            let Some(end) = self
                .bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            else {
                return;
            };
            self.frame_start = Some(end + 4);
        }
        let mut cursor = self.frame_start.unwrap_or(0);
        while let Some(first) = self.bytes.get(cursor).copied() {
            let Some(second) = self.bytes.get(cursor + 1).copied() else {
                break;
            };
            let opcode = first & 0x0f;
            let masked = second & 0x80 != 0;
            let mut header = 2;
            let mut length = usize::from(second & 0x7f);
            if length == 126 {
                let Some(extended) = self.bytes.get(cursor + 2..cursor + 4) else {
                    break;
                };
                length = usize::from(u16::from_be_bytes([extended[0], extended[1]]));
                header += 2;
            } else if length == 127 {
                let Some(extended) = self.bytes.get(cursor + 2..cursor + 10) else {
                    break;
                };
                let Ok(length_bytes) = <[u8; 8]>::try_from(extended) else {
                    break;
                };
                let Ok(length_value) = usize::try_from(u64::from_be_bytes(length_bytes)) else {
                    break;
                };
                length = length_value;
                header += 8;
            }
            let mask_start = cursor + header;
            if masked && self.bytes.len() < mask_start + 4 {
                break;
            }
            let payload_start = mask_start + if masked { 4 } else { 0 };
            let Some(payload_end) = payload_start.checked_add(length) else {
                break;
            };
            let Some(payload) = self.bytes.get(payload_start..payload_end) else {
                break;
            };
            if opcode == 1 {
                let decoded = if masked {
                    let mask = &self.bytes[mask_start..payload_start];
                    payload
                        .iter()
                        .enumerate()
                        .map(|(index, byte)| *byte ^ mask[index % 4])
                        .collect()
                } else {
                    payload.to_vec()
                };
                if let Ok(text) = String::from_utf8(decoded) {
                    self.messages.push(text);
                }
            }
            cursor = payload_end;
            self.frame_start = Some(cursor);
        }
    }
}

#[derive(Clone, Copy)]
enum TraceDirection {
    ClientToServer,
    ServerToClient,
}

fn subscription_cursor(message: &str) -> Option<(u64, u64)> {
    let value = sonic_rs::from_str::<Value>(message).ok()?;
    if value.get("method").and_then(|field| field.as_str()) != Some("session/subscribe") {
        return None;
    }
    let params = value.get("params")?;
    Some((params.get("gen")?.as_u64()?, params.get("after")?.as_u64()?))
}

fn update_cursor(message: &str) -> Option<(u64, u64)> {
    let value = sonic_rs::from_str::<Value>(message).ok()?;
    if value.get("method").and_then(|field| field.as_str()) != Some("session/update") {
        return None;
    }
    let params = value.get("params")?;
    Some((params.get("gen")?.as_u64()?, params.get("seq")?.as_u64()?))
}

/// Presents the serve port in the `Host` header of the upgrade request: the
/// client dialed the proxy port, but the loopback host guard only accepts a
/// host header that names the serve port.
fn rewrite_host_header(request: &[u8], serve_port: u16) -> Vec<u8> {
    let text = String::from_utf8_lossy(request);
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive("\r\n") {
        if line
            .get(..5)
            .is_some_and(|head| head.eq_ignore_ascii_case("host:"))
        {
            let _ = write!(out, "Host: 127.0.0.1:{serve_port}\r\n");
        } else {
            out.push_str(line);
        }
    }
    out.into_bytes()
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "arguments are moved into the spawned relay thread"
)]
fn forward(
    mut reader: TcpStream,
    mut writer: TcpStream,
    capture: Option<(Arc<Mutex<FrameCapture>>, TraceDirection)>,
    serve_port: Option<u16>,
) {
    let mut buffer = [0_u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut upgrading = serve_port.is_some();
    loop {
        let length = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if let Some((capture, direction)) = &capture {
            let mut capture = capture
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match direction {
                TraceDirection::ClientToServer => capture.push_client(&buffer[..length]),
                TraceDirection::ServerToClient => capture.push_server(&buffer[..length]),
            }
        }
        let chunk = if upgrading {
            pending.extend_from_slice(&buffer[..length]);
            let Some(header_end) = pending.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let mut rewritten =
                rewrite_host_header(&pending[..header_end + 4], serve_port.unwrap_or(0));
            rewritten.extend_from_slice(&pending[header_end + 4..]);
            pending.clear();
            upgrading = false;
            rewritten
        } else {
            buffer[..length].to_vec()
        };
        if writer.write_all(&chunk).is_err() {
            break;
        }
    }
    if upgrading && !pending.is_empty() {
        let _ = writer.write_all(&pending);
    }
    let _ = writer.shutdown(Shutdown::Write);
}
