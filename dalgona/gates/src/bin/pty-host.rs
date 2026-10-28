// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Private PTY host helper for the dalgona-gates keypress probe.
//!
//! Unpublished gates-only binary: it never ships, it is no product CLI, and
//! it is no part of any release archive. The probe parent in
//! `dalgona_gates::pty::windows` launches this process inside a
//! kill-on-close job object and drives the real TUI child through the
//! bounded frame protocol in [`dalgona_gates::pty::protocol`].
//!
//! Every thread of the `ConPTY` session lives here, inside one killable OS
//! process under the parent's job: the helper's reader, writer, and watcher
//! threads join on the normal path, and a parent timeout kills the whole
//! job as the backstop. On non-Windows hosts the helper exits with a typed
//! unsupported-host failure: the `ConPTY` half of the probe only exists on
//! Windows, and a silent exit or a fake success is never acceptable.
//!
//! The helper captures its wire exactly once at process start through
//! `dalgon::raw_wire_streams`: the returned reader feeds binary
//! `read_request` unchanged, and the returned writer is shared behind one
//! mutex so every complete response frame leaves under a single guard. No
//! ambient `std::io::stdin`/`stdout` handle is consulted; the protocol
//! bytes themselves are unchanged.
//!
//! Shutdown is unified: the frame loop never returns directly. Every
//! outcome (shutdown, cancel, violation) breaks to one shutdown section
//! that kills the child tree, closes the writer channel, and joins all
//! three threads under a shared deadline, so the pseudoconsole controller
//! and child handles always outlive the pipe drain.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args();
    let _program = arguments.next();
    if arguments.next().is_some() {
        eprintln!(
            "dalgona-pty-host takes no arguments; the probe parent drives it over stdin frames"
        );
        return ExitCode::FAILURE;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("dalgona-pty-host failed: {failure}");
            ExitCode::FAILURE
        }
    }
}

/// Serves frame traffic on Windows; on any other host this is the explicit
/// unsupported-host boundary, not a stub and not a pass.
#[cfg(windows)]
fn run() -> Result<(), String> {
    host::serve_frames()
}

/// The typed unsupported-host failure for non-Windows hosts.
#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err(format!(
        "unsupported host: the ConPTY keypress probe helper requires Windows, and this \
         host is {}; run the keypress gate on the windows-2022 or windows-11-arm native \
         lanes instead",
        std::env::consts::OS
    ))
}

#[cfg(windows)]
mod host {
    //! The `ConPTY` host loop: frame-driven, thread-joined, never unbounded.

    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    use conpty_oxide::blocking::{Child, Command};
    use conpty_oxide::{ConPtyBackend, SessionOptions, Size};

    use dalgona_gates::pty::protocol::{self, Request, Response, SpawnRequest};

    /// Bound on each helper thread's join during shutdown.
    const JOIN_LIMIT: Duration = Duration::from_secs(5);
    /// Poll interval while watching the child's exit.
    const WATCH_POLL: Duration = Duration::from_millis(5);

    /// How the frame loop ended; the single shutdown section below maps
    /// each outcome to its frames without ever returning mid-loop.
    enum LoopEnd {
        /// The parent sent `Shutdown`: answer with `Final`.
        Shutdown,
        /// The parent hung up its stdin: clean up and fail, never succeed.
        Cancelled,
        /// A protocol, transport, or thread-startup violation: answer with
        /// `Failed`, no `Final`.
        Broken(String),
    }

    /// Validates the `Spawn` request and opens the pseudoconsole session.
    ///
    /// # Errors
    /// Returns a `Failed`-framed protocol, backend, or spawn failure; every
    /// text is emitted through `writer` before the error returns.
    fn open_console(
        spawn: &SpawnRequest,
        writer: &Mutex<std::fs::File>,
    ) -> Result<(conpty_oxide::blocking::SessionParts, u32), String> {
        let initial_size = Size::try_new(spawn.cols, spawn.rows).map_err(|error| {
            fatal(
                format!(
                    "protocol: invalid window size {}x{}: {error}",
                    spawn.cols, spawn.rows
                ),
                writer,
            )
        })?;
        let backend = ConPtyBackend::system().map_err(|error| {
            fatal(
                format!("the system ConPTY backend is unavailable: {error}"),
                writer,
            )
        })?;
        let options = SessionOptions::new().size(initial_size).backend(backend);
        let mut command = Command::new(&spawn.program);
        command
            .args(&spawn.args)
            .env_clear()
            .envs(
                spawn
                    .env
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .current_dir(&spawn.cwd);
        let session = command.spawn_with(options).map_err(|error| {
            fatal(
                format!("spawning {} failed: {error}", spawn.program),
                writer,
            )
        })?;
        let parts = session.into_parts();
        let pid = parts.child.id();
        Ok((parts, pid))
    }

    /// Starts the three session threads and returns which ones started.
    ///
    /// A failed spawn never returns directly: the error becomes a `Broken`
    /// outcome for the caller, and the name list records only the threads
    /// that actually started so partial startup joins exactly those.
    fn start_threads(
        parts: conpty_oxide::blocking::SessionParts,
        done_tx: &mpsc::SyncSender<(&'static str, Result<(), String>)>,
        write_rx: mpsc::Receiver<Vec<u8>>,
        writer: &Arc<Mutex<std::fs::File>>,
        ended: &Arc<AtomicBool>,
        epoch: Instant,
    ) -> (
        Arc<Mutex<Child>>,
        conpty_oxide::PtyController,
        Vec<&'static str>,
        Option<String>,
    ) {
        let conpty_oxide::blocking::SessionParts {
            child,
            output,
            input: console_input,
            controller,
            ..
        } = parts;
        let child = Arc::new(Mutex::new(child));
        let mut started_names: Vec<&'static str> = Vec::new();
        let mut startup_error: Option<String> = None;
        match spawn_writer(done_tx, write_rx, console_input, Arc::clone(writer), epoch) {
            Ok(()) => started_names.push("H-writer"),
            Err(error) => startup_error = Some(error),
        }
        if startup_error.is_none() {
            match spawn_reader(done_tx, output, Arc::clone(writer), epoch) {
                Ok(()) => started_names.push("H-reader"),
                Err(error) => startup_error = Some(error),
            }
        }
        if startup_error.is_none() {
            match spawn_watcher(
                done_tx,
                Arc::clone(&child),
                Arc::clone(writer),
                Arc::clone(ended),
            ) {
                Ok(()) => started_names.push("H-watch"),
                Err(error) => startup_error = Some(error),
            }
        }
        (child, controller, started_names, startup_error)
    }

    /// Consumes request frames until a Shutdown, a parent hangup, or a
    /// protocol/transport failure; the loop never returns early so the
    /// caller's one shutdown section owns kill, channel close, and join.
    fn frame_loop(
        input: &mut impl Read,
        write_tx: &mpsc::SyncSender<Vec<u8>>,
        child: &Mutex<Child>,
        ended: &AtomicBool,
        controller: &conpty_oxide::PtyController,
    ) -> LoopEnd {
        loop {
            match read_request(input) {
                Err(reason) => break LoopEnd::Broken(reason),
                Ok(Some(Request::Write { bytes })) => {
                    if bytes.is_empty() {
                        break LoopEnd::Broken(
                            "protocol: a Write frame carried no bytes".to_owned(),
                        );
                    }
                    if write_tx.send(bytes).is_err() {
                        break LoopEnd::Broken(
                            "the H-writer thread stopped accepting input".to_owned(),
                        );
                    }
                }
                Ok(Some(Request::Resize { cols, rows })) => match Size::try_new(cols, rows) {
                    Ok(size) => {
                        if let Err(error) = controller.resize(size) {
                            break LoopEnd::Broken(format!(
                                "resizing the pseudoconsole failed: {error}"
                            ));
                        }
                    }
                    Err(error) => {
                        break LoopEnd::Broken(format!(
                            "protocol: invalid resize {cols}x{rows}: {error}"
                        ));
                    }
                },
                Ok(Some(Request::Kill)) => {
                    if let Err(reason) = kill_child(child, ended) {
                        break LoopEnd::Broken(reason);
                    }
                }
                Ok(Some(Request::Shutdown)) => break LoopEnd::Shutdown,
                Ok(Some(Request::Spawn(_))) => {
                    break LoopEnd::Broken(
                        "protocol: the parent sent a second Spawn frame".to_owned(),
                    );
                }
                Ok(None) => break LoopEnd::Cancelled,
            }
        }
    }

    /// Joins each started thread under one shared deadline and reports the
    /// names that missed it plus the first thread failure.
    fn join_threads(
        started_names: &[&'static str],
        done_rx: &mpsc::Receiver<(&'static str, Result<(), String>)>,
        deadline: Instant,
    ) -> (Vec<String>, Option<String>) {
        let mut joined: Vec<&'static str> = Vec::new();
        let mut thread_failure: Option<String> = None;
        for _ in 0..started_names.len() {
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                break;
            }
            match done_rx.recv_timeout(wait) {
                Ok((name, Ok(()))) => joined.push(name),
                Ok((name, Err(reason))) => {
                    joined.push(name);
                    if thread_failure.is_none() {
                        thread_failure = Some(format!("{name}: {reason}"));
                    }
                }
                Err(_) => break,
            }
        }
        let unjoined = started_names
            .iter()
            .filter(|name| !joined.contains(name))
            .map(|name| (*name).to_owned())
            .collect();
        (unjoined, thread_failure)
    }

    /// Maps the loop outcome to its terminal frames: `Final` answers
    /// `Shutdown`, a cancelled or broken loop reports failure, and a
    /// `Broken` outcome always emits `Failed` before the error returns.
    fn report_end(
        end: LoopEnd,
        unjoined: Vec<String>,
        thread_failure: Option<String>,
        kill_error: Option<String>,
        writer: &Mutex<std::fs::File>,
    ) -> Result<(), String> {
        match end {
            LoopEnd::Shutdown => {
                if !unjoined.is_empty() {
                    let _ = emit(
                        &Response::Final {
                            unjoined: unjoined.clone(),
                        },
                        writer,
                    );
                    return Err(format!(
                        "helper threads missed their join deadline: {unjoined:?}"
                    ));
                }
                if let Some(failure) = thread_failure {
                    let _ = emit(
                        &Response::Final {
                            unjoined: Vec::new(),
                        },
                        writer,
                    );
                    return Err(failure);
                }
                if let Some(reason) = kill_error {
                    return Err(fatal(reason, writer));
                }
                emit(
                    &Response::Final {
                        unjoined: Vec::new(),
                    },
                    writer,
                )
            }
            LoopEnd::Cancelled => {
                let _ = emit(&Response::Final { unjoined }, writer);
                match kill_error {
                    None => Err("the parent hung up; the helper cleaned up and exited".to_owned()),
                    Some(kill) => Err(format!(
                        "the parent hung up; the helper cleaned up and exited; {kill}"
                    )),
                }
            }
            LoopEnd::Broken(reason) => {
                let mut full = reason;
                if !unjoined.is_empty() {
                    full =
                        format!("{full}; helper threads missed their join deadline: {unjoined:?}");
                }
                if let Some(failure) = thread_failure {
                    full = format!("{full}; helper thread failed: {failure}");
                }
                if let Some(kill) = kill_error {
                    full = format!("{full}; {kill}");
                }
                let _ = emit(
                    &Response::Failed {
                        reason: full.clone(),
                    },
                    writer,
                );
                Err(full)
            }
        }
    }

    /// Runs the whole session: one Spawn, then frames, then a joined,
    /// reported shutdown.
    ///
    /// The wire is captured exactly once at the top through
    /// `dalgon::raw_wire_streams`: the reader feeds binary `read_request`
    /// unchanged, and the writer is shared behind one mutex so each
    /// complete frame leaves under a single guard.
    ///
    /// # Errors
    /// Returns protocol violations, spawn failures, and shutdown failures;
    /// every fatal path first reports `Failed` to the parent when its
    /// pipes still exist.
    pub(super) fn serve_frames() -> Result<(), String> {
        let epoch = Instant::now();
        let streams = dalgon::raw_wire_streams()
            .map_err(|error| format!("capturing raw wire streams failed: {error}"))?;
        let mut input = streams.reader;
        let writer = Arc::new(Mutex::new(streams.writer));
        let spawn = match read_request(&mut input).map_err(|reason| fatal(reason, &writer))? {
            Some(Request::Spawn(spawn)) => spawn,
            Some(other) => {
                return Err(fatal(
                    format!(
                        "protocol: expected Spawn first, got {}",
                        protocol::tag_name(other.tag())
                    ),
                    &writer,
                ));
            }
            None => return Err("protocol: the parent hung up before Spawn".to_owned()),
        };
        let (parts, pid) = open_console(&spawn, &writer)?;
        // Set once by the watcher when `try_wait` observes the child's
        // exit: only an observed exit proves the tree is done. Console EOF
        // is not proof (a detached child can outlive its pipes), so the
        // reader drains without touching this flag. Shutdown always runs
        // the kill; a kill error after the observed end is tolerated.
        let ended = Arc::new(AtomicBool::new(false));
        let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(8);
        let (done_tx, done_rx) = mpsc::sync_channel::<(&'static str, Result<(), String>)>(3);

        // The Spawned frame is emitted before the threads start, and a
        // failed spawn never returns directly: the error becomes a `Broken`
        // outcome below and only the threads that actually started are
        // joined, so partial startup runs the same kill, channel-close, and
        // bounded-join path as the loop.
        let mut startup_error: Option<String> = None;
        if let Err(error) = emit(&Response::Spawned { pid }, &writer) {
            startup_error = Some(format!("writing the Spawned frame failed: {error}"));
        }
        let (child, controller, started_names, thread_startup_error) = if startup_error.is_none() {
            start_threads(parts, &done_tx, write_rx, &writer, &ended, epoch)
        } else {
            (
                Arc::new(Mutex::new(parts.child)),
                parts.controller,
                Vec::new(),
                startup_error.take(),
            )
        };
        if let Some(error) = thread_startup_error {
            startup_error = Some(error);
        }
        drop(done_tx);

        // The loop never returns: every arm breaks with an outcome so the
        // single shutdown section below owns the kill, the channel close,
        // and the bounded join. Returning directly would drop the
        // controller and child handles while the reader, writer, and
        // watcher threads still hold the pipes. A thread-startup failure
        // skips the loop and lands in the same shutdown section as a
        // `Broken` outcome with only the started threads joined.
        let end: LoopEnd = match startup_error {
            Some(error) => LoopEnd::Broken(error),
            None => frame_loop(&mut input, &write_tx, &child, &ended, &controller),
        };

        // One shutdown for every outcome: stop the writer, terminate any
        // live child tree, then join every started thread under one shared
        // deadline (three names on the loop path, fewer when a thread spawn
        // failed). The controller and child handles drop only after the
        // join, so the pipes drain before any console teardown runs; on a
        // missed deadline the controller's drop is suppressed instead.
        drop(write_tx);
        let kill_error: Option<String> = kill_child(&child, &ended).err();
        let (unjoined, thread_failure) =
            join_threads(&started_names, &done_rx, Instant::now() + JOIN_LIMIT);
        if !unjoined.is_empty() {
            // The drain missed its deadline with a thread still holding the
            // pipes; dropping the controller here could block console
            // teardown without bound. Suppress its drop instead: process
            // exit and the parent's job kill stay the backstops.
            let _no_teardown = std::mem::ManuallyDrop::new(controller);
        }
        report_end(end, unjoined, thread_failure, kill_error, &writer)
    }

    /// Starts the writer thread: it owns the console input half, stamps
    /// immediately before each write, and reports one `Wrote` per item.
    ///
    /// `writer` is the shared wire writer: each `Wrote` or `Failed` frame
    /// leaves under that one mutex guard.
    fn spawn_writer<W>(
        done_tx: &mpsc::SyncSender<(&'static str, Result<(), String>)>,
        write_rx: mpsc::Receiver<Vec<u8>>,
        mut console_input: W,
        writer: Arc<Mutex<std::fs::File>>,
        epoch: Instant,
    ) -> Result<(), String>
    where
        W: Write + Send + 'static,
    {
        let done_tx = done_tx.clone();
        std::thread::Builder::new()
            .name("H-writer".to_owned())
            .spawn(move || {
                let outcome = loop {
                    match write_rx.recv() {
                        Ok(bytes) => {
                            let t_ns = stamp_ns(epoch);
                            let written = console_input
                                .write_all(&bytes)
                                .and_then(|()| console_input.flush());
                            if let Err(error) = written {
                                let reason = format!("writing the console input failed: {error}");
                                let _ = emit(
                                    &Response::Failed {
                                        reason: reason.clone(),
                                    },
                                    &writer,
                                );
                                break Err(reason);
                            }
                            if let Err(error) = emit(&Response::Wrote { t_ns }, &writer) {
                                break Err(error);
                            }
                        }
                        Err(_) => break Ok(()),
                    }
                };
                drop(console_input);
                let _ = done_tx.send(("H-writer", outcome));
            })
            .map(|_| ())
            .map_err(|error| format!("spawning the H-writer thread failed: {error}"))
    }

    /// Starts the reader thread: it stamps immediately after each read and
    /// forwards chunks of at most [`protocol::MAX_BYTES`].
    ///
    /// `writer` is the shared wire writer: each `Out` or `Failed` frame
    /// leaves under that one mutex guard.
    fn spawn_reader<R>(
        done_tx: &mpsc::SyncSender<(&'static str, Result<(), String>)>,
        mut output: R,
        writer: Arc<Mutex<std::fs::File>>,
        epoch: Instant,
    ) -> Result<(), String>
    where
        R: Read + Send + 'static,
    {
        let done_tx = done_tx.clone();
        std::thread::Builder::new()
            .name("H-reader".to_owned())
            .spawn(move || {
                let mut chunk = [0_u8; protocol::MAX_BYTES];
                let outcome = loop {
                    match output.read(&mut chunk) {
                        Ok(0) => break Ok(()),
                        Ok(read) => {
                            let t_ns = stamp_ns(epoch);
                            let frame = Response::Out {
                                t_ns,
                                bytes: chunk[..read].to_vec(),
                            };
                            if let Err(error) = emit(&frame, &writer) {
                                break Err(error);
                            }
                        }
                        Err(error) if is_console_end(&error) => break Ok(()),
                        Err(error) => {
                            let reason = format!("reading the console output failed: {error}");
                            let _ = emit(
                                &Response::Failed {
                                    reason: reason.clone(),
                                },
                                &writer,
                            );
                            break Err(reason);
                        }
                    }
                };
                drop(output);
                let _ = done_tx.send(("H-reader", outcome));
            })
            .map(|_| ())
            .map_err(|error| format!("spawning the H-reader thread failed: {error}"))
    }

    /// Starts the watcher thread: it polls the child's exit every
    /// [`WATCH_POLL`], reports `Exited` once, and never blocks unboundedly.
    ///
    /// `writer` is the shared wire writer: the `Exited` frame leaves under
    /// that one mutex guard.
    fn spawn_watcher(
        done_tx: &mpsc::SyncSender<(&'static str, Result<(), String>)>,
        child: Arc<Mutex<Child>>,
        writer: Arc<Mutex<std::fs::File>>,
        ended: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let done_tx = done_tx.clone();
        std::thread::Builder::new()
            .name("H-watch".to_owned())
            .spawn(move || {
                let outcome = loop {
                    std::thread::sleep(WATCH_POLL);
                    let polled = child
                        .lock()
                        .map_err(|_| "the child state mutex was poisoned".to_owned())
                        .and_then(|mut guard| {
                            guard
                                .try_wait()
                                .map_err(|error| format!("polling the child failed: {error}"))
                        });
                    match polled {
                        Ok(Some(status)) => {
                            ended.store(true, Ordering::Release);
                            let frame = Response::Exited {
                                code: status.code(),
                            };
                            if let Err(error) = emit(&frame, &writer) {
                                break Err(error);
                            }
                            break Ok(());
                        }
                        Ok(None) => {}
                        Err(reason) => break Err(reason),
                    }
                };
                let _ = done_tx.send(("H-watch", outcome));
            })
            .map(|_| ())
            .map_err(|error| format!("spawning the H-watch thread failed: {error}"))
    }

    /// Terminates the child tree through its session job. The kill runs even
    /// after the watcher observed the root exit: descendants can outlive the
    /// root while holding the console pipes open, and killing a finished
    /// tree is a no-op success. A kill error after the observed end is the
    /// job's normal protest, not a failure.
    fn kill_child(child: &Mutex<Child>, ended: &AtomicBool) -> Result<(), String> {
        let mut guard = child
            .lock()
            .map_err(|_| "the child state mutex was poisoned".to_owned())?;
        match guard.kill() {
            Ok(()) => Ok(()),
            Err(error) => {
                if ended.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(format!("terminating the child tree failed: {error}"))
                }
            }
        }
    }

    /// Reads one request frame from the piped input; `None` at the end of
    /// the stream.
    ///
    /// Unchanged binary framing: the caller passes the captured reader
    /// `File`, and every header and payload bound behaves as before.
    ///
    /// # Errors
    /// Returns malformed-frame failures with their bounded context.
    fn read_request(stream: &mut impl Read) -> Result<Option<Request>, String> {
        let mut header = [0_u8; protocol::HEADER_LEN];
        let mut filled = 0_usize;
        while filled < protocol::HEADER_LEN {
            match stream.read(&mut header[filled..]) {
                Ok(0) => {
                    if filled == 0 {
                        return Ok(None);
                    }
                    return Err(format!(
                        "frame header truncated: received {filled} of {} bytes before EOF",
                        protocol::HEADER_LEN
                    ));
                }
                Ok(received) => filled += received,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("reading a frame header failed: {error}")),
            }
        }
        let tag = header[0];
        let length = protocol::payload_length(&header)?;
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).map_err(|error| {
            format!(
                "reading a {} frame payload failed: {error}",
                protocol::tag_name(tag)
            )
        })?;
        Request::decode(tag, &payload).map(Some)
    }

    /// Encodes one response frame, then writes and flushes it under a
    /// single mutex guard, so concurrent helper threads never interleave
    /// bytes from two frames.
    ///
    /// # Errors
    /// Returns the encode failure, a poisoned-mutex failure, or the write
    /// failure; cleanup-path callers may ignore it.
    fn emit(response: &Response, writer: &Mutex<std::fs::File>) -> Result<(), String> {
        let mut frame = Vec::new();
        response.encode(&mut frame).map_err(|error| {
            format!(
                "encoding the {} frame failed: {error}",
                protocol::tag_name(response.tag())
            )
        })?;
        let mut guard = writer
            .lock()
            .map_err(|_| "the wire writer mutex was poisoned".to_owned())?;
        guard
            .write_all(&frame)
            .and_then(|()| guard.flush())
            .map_err(|error| {
                format!(
                    "writing the {} frame failed: {error}",
                    protocol::tag_name(response.tag())
                )
            })
    }

    /// Reports `Failed` and returns the reason for the caller's own error.
    fn fatal(reason: String, writer: &Mutex<std::fs::File>) -> String {
        let _ = emit(
            &Response::Failed {
                reason: reason.clone(),
            },
            writer,
        );
        reason
    }

    /// Helper nanoseconds since `epoch`, saturated at `u64::MAX`.
    fn stamp_ns(epoch: Instant) -> u64 {
        u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Whether a console read error means the stream ended.
    fn is_console_end(error: &std::io::Error) -> bool {
        matches!(error.kind(), std::io::ErrorKind::BrokenPipe)
    }

    #[cfg(test)]
    mod read_request_witness {
        //! Deterministic counterexamples for the blocking `read_request` seam.
        //!
        //! No `ConPTY`, no helper process, no native I/O: `std::io::Cursor`
        //! feeds immutable literal bytes through the real `read_request`
        //! parser, so the inputs cannot drift from the frozen wire. An empty
        //! stream is the only clean EOF; EOF after 1..4 of the 5 header bytes
        //! is a named truncation, never a silent close. One full-frame
        //! control pins the `Shutdown` literal against the shared encoder and
        //! proves a complete frame still decodes.

        use super::read_request;
        use dalgona_gates::pty::protocol::Request;
        use std::io::Cursor;

        /// Frozen `Shutdown` frame: tag `5` plus a zero `u32` length.
        const SHUTDOWN: [u8; 5] = [5, 0, 0, 0, 0];

        /// Whether a refusal names truncation.
        fn is_truncation(reason: &str) -> bool {
            reason.contains("truncat") || reason.contains("Truncat")
        }

        /// An empty stream ends cleanly with no frame.
        #[test]
        fn zero_byte_eof_is_clean_close() -> Result<(), String> {
            let mut empty = Cursor::new(Vec::new());
            match read_request(&mut empty) {
                Ok(None) => Ok(()),
                Ok(Some(_)) => Err("empty stream decoded a frame, want clean EOF".to_owned()),
                Err(reason) => Err(format!(
                    "empty stream failed, want clean EOF, got: {reason}"
                )),
            }
        }

        /// One byte of the 5-byte header is truncation, never clean EOF.
        #[test]
        fn partial_header_1_byte_is_truncation() -> Result<(), String> {
            let mut stream = Cursor::new(SHUTDOWN[..1].to_vec());
            match read_request(&mut stream) {
                Ok(None) => Err("1-byte header reported clean EOF, want truncation".to_owned()),
                Ok(Some(_)) => Err("1-byte header gave a frame, want truncation".to_owned()),
                Err(reason) => {
                    if !is_truncation(&reason) {
                        return Err(format!(
                            "1-byte header refusal lacks truncation, got: {reason}"
                        ));
                    }
                    Ok(())
                }
            }
        }

        /// Two bytes of the 5-byte header is truncation, never clean EOF.
        #[test]
        fn partial_header_2_bytes_is_truncation() -> Result<(), String> {
            let mut stream = Cursor::new(SHUTDOWN[..2].to_vec());
            match read_request(&mut stream) {
                Ok(None) => Err("2-byte header reported clean EOF, want truncation".to_owned()),
                Ok(Some(_)) => Err("2-byte header gave a frame, want truncation".to_owned()),
                Err(reason) => {
                    if !is_truncation(&reason) {
                        return Err(format!(
                            "2-byte header refusal lacks truncation, got: {reason}"
                        ));
                    }
                    Ok(())
                }
            }
        }

        /// Three bytes of the 5-byte header is truncation, never clean EOF.
        #[test]
        fn partial_header_3_bytes_is_truncation() -> Result<(), String> {
            let mut stream = Cursor::new(SHUTDOWN[..3].to_vec());
            match read_request(&mut stream) {
                Ok(None) => Err("3-byte header reported clean EOF, want truncation".to_owned()),
                Ok(Some(_)) => Err("3-byte header gave a frame, want truncation".to_owned()),
                Err(reason) => {
                    if !is_truncation(&reason) {
                        return Err(format!(
                            "3-byte header refusal lacks truncation, got: {reason}"
                        ));
                    }
                    Ok(())
                }
            }
        }

        /// Four bytes of the 5-byte header is truncation, never clean EOF.
        #[test]
        fn partial_header_4_bytes_is_truncation() -> Result<(), String> {
            let mut stream = Cursor::new(SHUTDOWN[..4].to_vec());
            match read_request(&mut stream) {
                Ok(None) => Err("4-byte header reported clean EOF, want truncation".to_owned()),
                Ok(Some(_)) => Err("4-byte header gave a frame, want truncation".to_owned()),
                Err(reason) => {
                    if !is_truncation(&reason) {
                        return Err(format!(
                            "4-byte header refusal lacks truncation, got: {reason}"
                        ));
                    }
                    Ok(())
                }
            }
        }

        /// A complete `Shutdown` frame still decodes through the real parser.
        #[test]
        fn full_shutdown_frame_decodes() -> Result<(), String> {
            let mut encoded = Vec::new();
            Request::Shutdown
                .encode(&mut encoded)
                .map_err(|error| format!("encoding the Shutdown control failed: {error}"))?;
            if encoded != SHUTDOWN {
                return Err(format!(
                    "Shutdown control drifted from its literal: {encoded:?}"
                ));
            }
            let mut stream = Cursor::new(encoded);
            match read_request(&mut stream) {
                Ok(Some(Request::Shutdown)) => Ok(()),
                Ok(Some(other)) => Err(format!(
                    "full Shutdown frame decoded as {}, want Shutdown",
                    dalgona_gates::pty::protocol::tag_name(other.tag())
                )),
                Ok(None) => Err("full Shutdown frame reported clean EOF, want Shutdown".to_owned()),
                Err(reason) => Err(format!(
                    "full Shutdown frame failed, want Shutdown, got: {reason}"
                )),
            }
        }
    }
}
