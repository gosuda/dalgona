//! The Gemini CLI family (`google-gemini-cli`): a credential-less local subprocess transport.
//!
//! The child runs headless with a caller-supplied TOML deny-all policy and read-only approval mode.
//! Spawn owns one stream-owned polling round over stdin, stdout, stderr, and the leader.
//! Drop signals only when still armed; terminal paths terminate and await reaping.

use std::{
    collections::VecDeque,
    future::poll_fn,
    io,
    pin::Pin,
    process::{ExitStatus, Stdio},
    task::{Context, Poll},
};

use dal_core::{AssistantPart, ContextItem, Family, ModelRequest, Part};
use futures::{Stream, stream};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader, ReadBuf},
    process::{ChildStderr, ChildStdin, ChildStdout},
};

use super::shared;
use crate::{
    auth::credential::EnvSnapshot,
    error::ProviderError,
    family::process::is_process_gone,
    stream::{StopReason, StreamEvent},
};
use dal_core::Usage;

/// The `gemini` binary name; overridable per request for tests.
pub(crate) const BINARY: &str = "gemini";

/// The headless prompt flag: the directive below, with the transcript appended from stdin.
pub(crate) const PROMPT_FLAG: &str = "--prompt";

/// The fixed headless directive: the conversation arrives on stdin.
pub(crate) const DIRECTIVE: &str = "Answer using only the conversation on stdin.";

pub(crate) const OUTPUT_FORMAT_FLAG: &str = "--output-format";
pub(crate) const STREAM_JSON: &str = "stream-json";
pub(crate) const APPROVAL_MODE_FLAG: &str = "--approval-mode";

/// Read-only mode: the CLI may not write, and the deny-all policy below blocks every remaining tool.
pub(crate) const PLAN_MODE: &str = "plan";

pub(crate) const POLICY_FLAG: &str = "--policy";
pub(crate) const MODEL_FLAG: &str = "--model";

/// The largest stdout line accepted: a longer line ends the stream as a protocol error.
const LINE_LIMIT: usize = 1 << 20;

/// One pipe read chunk for the single-attempt polls below.
const READ_CHUNK: usize = 8192;

/// Everything one subprocess turn needs besides the neutral request.
pub(crate) struct CliRequest<'a> {
    /// The neutral request; its route id is the `--model` value. Carrying dal tools is refused.
    pub(crate) request: &'a ModelRequest,
    /// The binary to spawn; `None` runs `gemini` from `PATH`. This remains the provider-settings
    /// value at integration; the environment snapshot never selects the binary.
    pub(crate) binary: Option<&'a str>,
    /// Caller-created TOML deny-all policy path. Parent integration owns creation under
    /// `ProviderSet`/`data_dir`; this module only passes the supplied path through:
    /// ```toml
    /// [[rule]]
    /// toolName = "*"
    /// decision = "deny"
    /// priority = 100
    /// ```
    pub(crate) policy_file: &'a str,
    /// Captured working directory and environment view. Spawn clears ambient state and installs
    /// exactly this view. A missing captured working directory fails closed.
    pub(crate) env: &'a EnvSnapshot,
}

/// Renders one request as the transcript text fed to the CLI on stdin:
/// system instructions, then labeled turns. Assistant thinking replays as
/// context (it is the model's own prior reasoning); tool calls and results
/// replay as labeled lines so the CLI can continue the thread. Images are
/// not representable on this transport and are marked in place.
///
/// # Errors
///
/// [`ProviderError::UnresolvedBlob`] for a blob part.
pub(crate) fn render_prompt(request: &ModelRequest) -> Result<Vec<u8>, ProviderError> {
    let mut out = String::new();
    if !request.system.is_empty() {
        out.push_str("System instructions:\n");
        out.push_str(request.system.as_ref());
        out.push_str("\n\n");
    }
    out.push_str("Conversation:\n");
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } => {
                out.push_str("user:\n");
                render_parts(parts, &mut out)?;
            }
            ContextItem::Assistant { parts, .. } => {
                out.push_str("assistant:\n");
                for part in parts {
                    match part {
                        AssistantPart::Text { text, .. } | AssistantPart::Thinking { text, .. } => {
                            out.push_str(text.as_ref());
                            out.push('\n');
                        }
                        AssistantPart::ToolCall {
                            call, name, args, ..
                        } => {
                            use std::fmt::Write as _;
                            let _ = writeln!(
                                out,
                                "tool call {} ({}): {}",
                                call.as_str(),
                                name.as_ref(),
                                args.as_str()
                            );
                        }
                    }
                }
            }
            ContextItem::ToolResult {
                call,
                name,
                is_error,
                parts,
            } => {
                use std::fmt::Write as _;
                let _ = writeln!(
                    out,
                    "tool result for {} ({}){}:",
                    call.as_str(),
                    name.as_ref(),
                    if *is_error { " [error]" } else { "" }
                );
                render_parts(parts, &mut out)?;
            }
        }
    }
    Ok(out.into_bytes())
}

fn render_parts(parts: &[Part], out: &mut String) -> Result<(), ProviderError> {
    for part in parts {
        match part {
            Part::Text { text } => {
                out.push_str(text.as_ref());
                out.push('\n');
            }
            Part::Image { mime, .. } => {
                use std::fmt::Write as _;
                let _ = writeln!(out, "[image {mime}: not sent to the subprocess]");
            }
            Part::Blob { blob_id, .. } => {
                return Err(ProviderError::UnresolvedBlob { blob_id: *blob_id });
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stream-json decoding
// ---------------------------------------------------------------------------

/// One stream-json line, tagged by `type` (`JsonStreamEventType` in the
/// pin). Unknown event types are ignored for forward compatibility; a known
/// action-bearing type in the wrong shape is a protocol error, while
/// informational events (`init`, `tool_result`) ignore payload members.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CliEvent {
    Init {},
    Message {
        role: String,
        content: String,
    },
    ToolUse {
        tool_name: String,
    },
    ToolResult {},
    Error {
        #[serde(default)]
        severity: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    Result {
        status: String,
        #[serde(default)]
        stats: Option<CliStats>,
        #[serde(default)]
        error: Option<CliErrorInfo>,
    },
}

/// The `type` member alone, for classifying undecodable lines.
#[derive(Deserialize)]
struct KindOnly {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

fn is_known_event(kind: &str) -> bool {
    matches!(
        kind,
        "init" | "message" | "tool_use" | "tool_result" | "error" | "result"
    )
}

#[derive(Deserialize, Default)]
struct CliStats {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached: u64,
}

#[derive(Deserialize)]
struct CliErrorInfo {
    /// The CLI's error class name (`getErrorType` in the pin), for example
    /// `FatalAuthenticationError`.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// The remedy appended to every CLI sign-in failure: dal holds no credential
/// for this surface, so the CLI's own store and the environment dal captured
/// for the child are the only places to fix it.
const SIGN_IN_REMEDY: &str = "Sign in with the gemini CLI itself, or set GEMINI_API_KEY (or the Vertex variables the CLI reads) in the environment dal captures.";

/// The error of a failed `result` event: a sign-in failure
/// (`FatalAuthenticationError`, `UnauthorizedError`) names the CLI-owned
/// remedy; every other class keeps the plain rendering. CLI `error` events
/// carry no error class, so they always keep the plain rendering.
fn cli_failure(kind: Option<&str>, message: Option<String>) -> ProviderError {
    let detail = message.unwrap_or_else(|| String::from("unknown error"));
    let message = if matches!(kind, Some("FatalAuthenticationError" | "UnauthorizedError")) {
        format!("gemini is not authenticated: {detail}. {SIGN_IN_REMEDY}")
    } else {
        format!("gemini-cli error: {detail}")
    };
    ProviderError::InvalidRequest { message }
}

/// The failure of a leader that exited non-zero without a `result` event,
/// by the CLI's documented exit codes (`ExitCodes` and the `Fatal*Error`
/// classes in the pin): 41 sign-in, 42 input, 52 configuration, 55 untrusted
/// workspace. Other codes stay a plain transport failure.
fn exit_failure(code: Option<i32>) -> Option<ProviderError> {
    let message = match code? {
        41 => format!("gemini is not authenticated (exit status 41). {SIGN_IN_REMEDY}"),
        42 => String::from("gemini rejected its input (exit status 42)"),
        52 => String::from("gemini reported a configuration error (exit status 52)"),
        55 => String::from(
            "gemini reported that the workspace is not trusted (exit status 55); trust the working directory in the gemini CLI",
        ),
        _ => return None,
    };
    Some(ProviderError::InvalidRequest { message })
}

/// The decoder of one subprocess transcript: stream-json lines in, neutral
/// events out. Assistant `message` events become text deltas; the terminal
/// is the `result` event — success emits the empty-call terminal triple,
/// failure ends the stream with that error. Anything else is informational.
#[derive(Debug, Default)]
struct CliDecoder {
    result_seen: bool,
}

impl CliDecoder {
    /// Decodes one stdout line, appending events to `out`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Protocol`] for a known event in the wrong shape or
    /// a second `result`; [`ProviderError::InvalidRequest`] for a CLI
    /// `error` event or a failed `result`. Lines after the result, blank
    /// lines, and unknown event types are ignored.
    fn feed_line(&mut self, line: &str, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        const FAMILY: Family = Family::GoogleGeminiCli;
        if line.trim().is_empty() {
            return Ok(());
        }
        let event: CliEvent = if let Ok(event) = sonic_rs::from_str(line) {
            event
        } else {
            let kind: Option<KindOnly> = sonic_rs::from_str(line).ok();
            if let Some(kind) = kind.and_then(|kind| kind.kind)
                && is_known_event(&kind)
            {
                return Err(ProviderError::Protocol {
                    family: FAMILY,
                    detail: format!("gemini-cli {kind} event is not valid"),
                });
            }
            return Ok(());
        };
        if self.result_seen {
            return match event {
                CliEvent::Result { .. } => Err(ProviderError::Protocol {
                    family: FAMILY,
                    detail: String::from("gemini-cli sent a second result"),
                }),
                _ => Ok(()),
            };
        }
        match event {
            CliEvent::Init {} | CliEvent::ToolResult {} => Ok(()),
            CliEvent::Message { role, content } => {
                if role == "assistant" && !content.is_empty() {
                    // Every assistant message event carries its content as
                    // the next delta, whether the CLI marks it partial or
                    // whole (`delta?` in the pin).
                    out.push(StreamEvent::TextDelta { text: content });
                }
                Ok(())
            }
            CliEvent::ToolUse { tool_name } => {
                // CLI-internal execution under the deny-all policy this
                // should never fire; when it does it stays visible as
                // reasoning, never as a dal tool call.
                out.push(StreamEvent::ReasoningDelta {
                    text: format!("gemini-cli ran tool {tool_name}"),
                });
                Ok(())
            }
            CliEvent::Error { severity, message } => {
                if severity.as_deref() == Some("warning") {
                    return Ok(());
                }
                Err(ProviderError::InvalidRequest {
                    message: format!(
                        "gemini-cli error: {}",
                        message.unwrap_or_else(|| String::from("unknown error"))
                    ),
                })
            }
            CliEvent::Result {
                status,
                stats,
                error,
            } => {
                self.result_seen = true;
                match status.as_str() {
                    "success" => {
                        let stats = stats.unwrap_or_default();
                        out.push(StreamEvent::ToolCallsDone { calls: Vec::new() });
                        out.push(StreamEvent::Usage {
                            usage: Usage {
                                input_tokens: stats.input_tokens,
                                cached_input_tokens: stats.cached.min(stats.input_tokens),
                                output_tokens: stats.output_tokens,
                                reasoning_tokens: None,
                                cache_write_tokens: 0,
                                premium_requests: None,
                                cost_usd: None,
                            },
                        });
                        out.push(StreamEvent::Stop {
                            reason: StopReason::EndTurn,
                        });
                        Ok(())
                    }
                    "error" => {
                        let (kind, message) =
                            error.map_or((None, None), |error| (error.kind, error.message));
                        Err(cli_failure(kind.as_deref(), message))
                    }
                    other => Err(ProviderError::Protocol {
                        family: FAMILY,
                        detail: format!("gemini-cli result status is not valid: {other}"),
                    }),
                }
            }
        }
    }

    /// Ends the transcript: without a `result` event the stream was cut.
    ///
    /// # Errors
    ///
    /// [`ProviderError::StreamCut`] when no `result` arrived.
    fn end(&mut self) -> Result<(), ProviderError> {
        if self.result_seen {
            return Ok(());
        }
        Err(ProviderError::StreamCut)
    }
}

// ---------------------------------------------------------------------------
// Subprocess driver
// ---------------------------------------------------------------------------

/// One completed transition from the stream-owned polling round. Byte counts and buffered bytes
/// apply at poll time and never travel in a message, so no droppable future ever holds progress.
enum CliMsg {
    /// A stdin write or shutdown failed; the detail never carries a secret.
    StdinFault(String),
    /// Stdin closed after the whole prompt reached the child.
    StdinClosed,
    /// One stdout line, newline stripped.
    StdoutLine(String),
    /// A stdout read, limit, or encoding failure.
    StdoutFault(String),
    /// Stdout reached EOF.
    StdoutEof,
    /// Stderr reached EOF or became unreadable; it is informational only.
    StderrEof,
    /// The leader exited and surviving pipe holders were signaled.
    Leader(ExitStatus),
    /// The leader could not be observed.
    LeaderFailed(String),
}

/// One spawned turn: the guarded child and the stream-owned driver feeding [`into_events`].
///
/// Dropping the stream signals only when still armed, so cancellation never blocks. Terminal
/// paths terminate the group or job and await reaping before completing.
pub(crate) struct CliStream {
    state: CliState,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "subprocess stream driver state tracks multiple IO and termination flags"
)]
struct CliState {
    guard: ChildGuard,
    sweep_error: Option<String>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Stdout bytes read but not yet delivered as a line.
    stdout_buf: Vec<u8>,
    stderr: ChildStderr,
    prompt: Option<Vec<u8>>,
    stdin_written: usize,
    decoder: CliDecoder,
    queue: VecDeque<Result<StreamEvent, ProviderError>>,
    writer_error: Option<String>,
    /// Stderr bytes drained, counted for diagnostics but never retained.
    stderr_bytes: usize,
    stdout_eof: bool,
    stderr_eof: bool,
    leader: Option<ExitStatus>,
    terminal_cleanup_done: bool,
    ended: bool,
}

/// Owns the group/job child. Drop signals termination only when still armed;
/// reaping happens on the explicit `.await` paths, which disarm on confirmed
/// cleanup so a later drop never signals a re-used process group or job.
struct ChildGuard {
    child: Box<dyn ChildWrapper>,
    armed: bool,
}

impl ChildGuard {
    fn new(child: Box<dyn ChildWrapper>) -> Self {
        Self { child, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn kill_attempt(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        self.child.start_kill()
    }

    /// Disarms only on confirmed cleanup (reaped or already gone); other
    /// failures stay armed so Drop retries the signal.
    async fn reap_attempt(&mut self) -> io::Result<()> {
        match self.child.wait().await {
            Ok(_) => {
                self.disarm();
                Ok(())
            }
            Err(error) if is_process_gone(&error) => {
                self.disarm();
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.start_kill();
        }
    }
}

/// Chains a recorded stdin write fault onto a terminal process failure.
///
/// The writer detail is an OS error string only, never prompt bytes or
/// subprocess output, so appending it leaks no secret. The process failure
/// kind is preserved: a sign-in `InvalidRequest` stays `InvalidRequest` so
/// retry routing is unchanged.
fn with_writer_cause(error: ProviderError, writer: &str) -> ProviderError {
    match error {
        ProviderError::InvalidRequest { message } => ProviderError::InvalidRequest {
            message: format!("{message}; writing the gemini prompt failed: {writer}"),
        },
        ProviderError::Transport { family, reason } => ProviderError::Transport {
            family,
            reason: format!("{reason}; writing the gemini prompt failed: {writer}"),
        },
        other => other,
    }
}

fn combine_with_cleanup(original: ProviderError, cleanup: &str) -> ProviderError {
    match original {
        ProviderError::Transport { family, reason } => ProviderError::Transport {
            family,
            reason: format!("{reason} (cleanup: {cleanup})"),
        },
        other => other,
    }
}

fn cleanup_transport(cleanup: &str) -> ProviderError {
    ProviderError::Transport {
        family: Family::GoogleGeminiCli,
        reason: format!("gemini cleanup failed: {cleanup}"),
    }
}

/// Best-effort termination, then reap. An already-gone child or group
/// (`is_process_gone`) is benign. Other cleanup failures return kind-only
/// text, never subprocess output. Disarms only on confirmed cleanup.
async fn terminate_and_reap(guard: &mut ChildGuard) -> Option<String> {
    let kill = match guard.kill_attempt() {
        Ok(()) => None,
        Err(error) if is_process_gone(&error) => None,
        Err(error) => Some(format!("kill {}", error.kind())),
    };
    match guard.reap_attempt().await {
        Ok(()) => kill,
        Err(error) => {
            let reap = format!("reap {}", error.kind());
            Some(match kill {
                Some(kind) => format!("{kind}; {reap}"),
                None => reap,
            })
        }
    }
}

/// Spawns one headless `gemini` turn.
///
/// The child receives exactly the supplied environment snapshot and captured working directory.
/// Ambient inheritance is cleared; dal forwards no secret of its own because this surface carries
/// no credential.
///
/// # Errors
///
/// [`ProviderError::InvalidRequest`] when the request carries dal tools, names a route of another
/// family, or lacks a captured working directory; [`ProviderError::UnresolvedBlob`] for a blob
/// part; [`ProviderError::Transport`] when the binary cannot spawn.
pub(crate) fn spawn(input: &CliRequest<'_>) -> Result<CliStream, ProviderError> {
    const FAMILY: Family = Family::GoogleGeminiCli;
    if !input.request.tools.is_empty() {
        return Err(ProviderError::InvalidRequest {
            message: String::from(
                "the google-gemini-cli family runs the gemini binary without dal tools; use a tool-capable Google family for tool calls",
            ),
        });
    }
    let model = shared::route_model(input.request, FAMILY)?;
    let prompt = render_prompt(input.request)?;
    let cwd = input
        .env
        .cwd()
        .ok_or_else(|| ProviderError::InvalidRequest {
            message: String::from(
                "the google-gemini-cli family requires a captured working directory",
            ),
        })?;
    let binary = input.binary.unwrap_or(BINARY);
    let mut command = CommandWrap::with_new(binary, |process| {
        process
            .arg(PROMPT_FLAG)
            .arg(DIRECTIVE)
            .arg(OUTPUT_FORMAT_FLAG)
            .arg(STREAM_JSON)
            .arg(APPROVAL_MODE_FLAG)
            .arg(PLAN_MODE)
            .arg(POLICY_FLAG)
            .arg(input.policy_file)
            .arg(MODEL_FLAG)
            .arg(model);
        process.current_dir(cwd);
        process.env_clear();
        process.envs(input.env.iter());
        process.stdin(Stdio::piped());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
    });
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.wrap(KillOnDrop);
    let child = command.spawn().map_err(|error| ProviderError::Transport {
        family: FAMILY,
        reason: format!("spawning {binary} failed: {error}"),
    })?;
    let mut guard = ChildGuard::new(child);
    let (Some(stdin), Some(stdout), Some(stderr)) = (
        guard.child.stdin().take(),
        guard.child.stdout().take(),
        guard.child.stderr().take(),
    ) else {
        let _ = guard.kill_attempt();
        return Err(ProviderError::Transport {
            family: FAMILY,
            reason: String::from("spawning gemini failed: a stdio pipe is missing"),
        });
    };
    Ok(CliStream {
        state: CliState {
            guard,
            sweep_error: None,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stdout_buf: Vec::new(),
            stderr,
            prompt: Some(prompt),
            stdin_written: 0,
            decoder: CliDecoder::default(),
            queue: VecDeque::new(),
            writer_error: None,
            stderr_bytes: 0,
            stdout_eof: false,
            stderr_eof: false,
            leader: None,
            terminal_cleanup_done: false,
            ended: false,
        },
    })
}

/// One poll attempt: idle (closed or pending), progress with no reportable event (poll again at
/// once — readiness was consumed), or a completed transition for the stream.
enum PollOutcome {
    Idle,
    Progress,
    Message(CliMsg),
}

/// One stdin write or shutdown attempt. Bytes apply before returning, so the outcome is always
/// consumed in the same poll and nothing lives in a droppable future.
fn poll_stdin(
    slot: &mut Option<ChildStdin>,
    prompt: Option<&[u8]>,
    written: &mut usize,
    cx: &mut Context<'_>,
) -> PollOutcome {
    let (Some(writer), Some(prompt)) = (slot.as_mut(), prompt) else {
        return PollOutcome::Idle;
    };
    if *written < prompt.len() {
        let outcome = match Pin::new(writer).poll_write(cx, &prompt[*written..]) {
            Poll::Ready(Ok(0)) => PollOutcome::Message(CliMsg::StdinFault(String::from(
                "writing the gemini prompt failed: no bytes were accepted",
            ))),
            Poll::Ready(Ok(count)) => {
                *written = written.saturating_add(count);
                PollOutcome::Progress
            }
            Poll::Ready(Err(error)) => PollOutcome::Message(CliMsg::StdinFault(error.to_string())),
            Poll::Pending => PollOutcome::Idle,
        };
        return outcome;
    }
    match Pin::new(writer).poll_shutdown(cx) {
        Poll::Ready(Ok(())) => PollOutcome::Message(CliMsg::StdinClosed),
        Poll::Ready(Err(error)) => PollOutcome::Message(CliMsg::StdinFault(error.to_string())),
        Poll::Pending => PollOutcome::Idle,
    }
}

/// One stdout read attempt into the state-owned line buffer. A complete line, EOF, or fault
/// reports; anything less buffers and asks for another poll at once.
fn poll_stdout(
    source: Option<(&mut BufReader<ChildStdout>, &mut Vec<u8>)>,
    cx: &mut Context<'_>,
) -> PollOutcome {
    let Some((reader, buf)) = source else {
        return PollOutcome::Idle;
    };
    if buf.len() > LINE_LIMIT {
        return PollOutcome::Message(CliMsg::StdoutFault(format!(
            "gemini-cli line exceeds {LINE_LIMIT} bytes"
        )));
    }
    let mut chunk = [0u8; READ_CHUNK];
    let mut incoming = ReadBuf::new(&mut chunk);
    match Pin::new(reader).poll_read(cx, &mut incoming) {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(error)) => {
            return PollOutcome::Message(CliMsg::StdoutFault(format!(
                "reading gemini stdout failed: {error}"
            )));
        }
        Poll::Pending => return PollOutcome::Idle,
    }
    let eof = incoming.filled().is_empty();
    if !eof {
        buf.extend_from_slice(incoming.filled());
    }
    if let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
        let mut line = buf.drain(..=end).collect::<Vec<u8>>();
        line.pop();
        if line.ends_with(b"\r") {
            line.pop();
        }
        // Enforce the declared per-line cap on the completed payload, not
        // just on the buffered prefix: the crossing chunk may already carry
        // the newline, in which case the entry and no-newline checks both
        // pass while the delivered line is over the limit.
        if line.len() > LINE_LIMIT {
            return PollOutcome::Message(CliMsg::StdoutFault(format!(
                "gemini-cli line exceeds {LINE_LIMIT} bytes"
            )));
        }
        return match String::from_utf8(line) {
            Ok(line) => PollOutcome::Message(CliMsg::StdoutLine(line)),
            Err(_) => PollOutcome::Message(CliMsg::StdoutFault(String::from(
                "gemini-cli stdout is not valid UTF-8",
            ))),
        };
    }
    if buf.len() > LINE_LIMIT {
        return PollOutcome::Message(CliMsg::StdoutFault(format!(
            "gemini-cli line exceeds {LINE_LIMIT} bytes"
        )));
    }
    if eof {
        if buf.is_empty() {
            return PollOutcome::Message(CliMsg::StdoutEof);
        }
        return match String::from_utf8(std::mem::take(buf)) {
            Ok(line) => PollOutcome::Message(CliMsg::StdoutLine(line)),
            Err(_) => PollOutcome::Message(CliMsg::StdoutFault(String::from(
                "gemini-cli stdout is not valid UTF-8",
            ))),
        };
    }
    PollOutcome::Progress
}

/// One stderr drain attempt. Drained bytes are counted, never retained; EOF reports.
fn poll_stderr(
    source: Option<&mut ChildStderr>,
    drained: &mut usize,
    cx: &mut Context<'_>,
) -> PollOutcome {
    let Some(slot) = source else {
        return PollOutcome::Idle;
    };
    let mut chunk = [0u8; READ_CHUNK];
    let mut incoming = ReadBuf::new(&mut chunk);
    match Pin::new(slot).poll_read(cx, &mut incoming) {
        Poll::Ready(Err(_)) => return PollOutcome::Message(CliMsg::StderrEof),
        Poll::Ready(Ok(())) => {}
        Poll::Pending => return PollOutcome::Idle,
    }
    let filled = incoming.filled();
    if filled.is_empty() {
        return PollOutcome::Message(CliMsg::StderrEof);
    }
    *drained = drained.saturating_add(filled.len());
    PollOutcome::Progress
}

/// Polls each open source once per poll and batches the completed transitions. Single
/// `poll_write`/`poll_read` attempts apply bytes immediately; the leader wait is the only future
/// kept across polls. No `select!`: tied-ready losers would drop counts and lines.
async fn drive_round(state: &mut CliState) -> Vec<CliMsg> {
    let CliState {
        guard,
        stdin,
        prompt,
        stdin_written,
        stdout,
        stdout_buf,
        stdout_eof,
        stderr,
        stderr_bytes,
        stderr_eof,
        leader,
        ..
    } = state;
    let mut leader_wait = leader.is_none().then(|| guard.child.inner_mut().wait());
    let mut leader_done = false;
    poll_fn(|cx| {
        if stdin.is_none() && *stdout_eof && *stderr_eof && leader.is_some() {
            return Poll::Ready(Vec::new());
        }
        let mut messages = Vec::new();
        loop {
            let mut quiet = true;
            for outcome in [
                poll_stdin(&mut *stdin, prompt.as_deref(), &mut *stdin_written, cx),
                poll_stdout(
                    (!*stdout_eof).then_some((&mut *stdout, &mut *stdout_buf)),
                    cx,
                ),
                poll_stderr(
                    (!*stderr_eof).then_some(&mut *stderr),
                    &mut *stderr_bytes,
                    cx,
                ),
            ] {
                match outcome {
                    PollOutcome::Idle => {}
                    PollOutcome::Progress => quiet = false,
                    PollOutcome::Message(message) => {
                        quiet = false;
                        messages.push(message);
                    }
                }
            }
            if !leader_done
                && leader.is_none()
                && let Some(wait) = leader_wait.as_mut()
            {
                match wait.as_mut().poll(cx) {
                    Poll::Ready(Ok(status)) => {
                        leader_done = true;
                        quiet = false;
                        messages.push(CliMsg::Leader(status));
                    }
                    Poll::Ready(Err(error)) => {
                        leader_done = true;
                        quiet = false;
                        messages.push(CliMsg::LeaderFailed(error.to_string()));
                    }
                    Poll::Pending => {}
                }
            }
            if !messages.is_empty() {
                return Poll::Ready(messages);
            }
            if quiet {
                return Poll::Pending;
            }
        }
    })
    .await
}

impl CliStream {
    /// Turns the spawned child into its neutral event stream. Dropping the stream signals only
    /// when still armed (see [`ChildGuard`]).
    pub(crate) fn into_events(
        self,
    ) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static {
        stream::unfold(self.state, |mut state| async move {
            loop {
                if let Some(item) = state.queue.pop_front() {
                    return Some((item, state));
                }
                if state.ended {
                    return None;
                }
                if state.leader.is_some()
                    && state.stdin.is_none()
                    && state.stdout_eof
                    && state.stderr_eof
                {
                    let result = state.finish_terminal();
                    settle(&mut state, Vec::new(), result).await;
                    continue;
                }
                let batch = drive_round(&mut state).await;
                for message in batch {
                    let mut out = Vec::new();
                    let result = state.handle(message, &mut out);
                    settle(&mut state, out, result).await;
                    if state.ended {
                        break;
                    }
                }
            }
        })
    }
}

/// Kills the group or job and awaits its reaping once the stream reaches a terminal state, then
/// queues the decoded events and terminal error. Runs before the terminal items are yielded.
async fn settle(state: &mut CliState, out: Vec<StreamEvent>, result: Result<(), ProviderError>) {
    if state.ended && !state.terminal_cleanup_done {
        state.terminal_cleanup_done = true;
        let mut cleanup = state.sweep_error.take();
        if let Some(late) = terminate_and_reap(&mut state.guard).await {
            cleanup = Some(match cleanup {
                Some(prior) => format!("{prior}; {late}"),
                None => late,
            });
        }
        match (result, cleanup) {
            (Ok(()), None) => state.queue.extend(out.into_iter().map(Ok)),
            (Ok(()), Some(cleanup)) => state.queue.push_back(Err(cleanup_transport(&cleanup))),
            (Err(error), None) => {
                state.queue.extend(out.into_iter().map(Ok));
                state.queue.push_back(Err(error));
            }
            (Err(error), Some(cleanup)) => {
                state.queue.extend(out.into_iter().map(Ok));
                state
                    .queue
                    .push_back(Err(combine_with_cleanup(error, &cleanup)));
            }
        }
        return;
    }
    state.queue.extend(out.into_iter().map(Ok));
    if let Err(error) = result {
        state.queue.push_back(Err(error));
    }
}

impl CliState {
    fn close_stdin(&mut self) {
        self.stdin = None;
        self.prompt = None;
        self.stdin_written = 0;
    }

    fn handle(&mut self, message: CliMsg, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        match message {
            CliMsg::StdinFault(detail) => {
                self.close_stdin();
                self.writer_error = Some(detail);
                Ok(())
            }
            CliMsg::StdinClosed => {
                self.close_stdin();
                Ok(())
            }
            CliMsg::StdoutLine(line) => self
                .decoder
                .feed_line(&line, out)
                .inspect_err(|_| self.ended = true),
            CliMsg::StdoutFault(detail) => {
                self.ended = true;
                Err(ProviderError::Protocol {
                    family: Family::GoogleGeminiCli,
                    detail,
                })
            }
            CliMsg::StdoutEof => {
                self.stdout_eof = true;
                Ok(())
            }
            CliMsg::StderrEof => {
                self.stderr_eof = true;
                Ok(())
            }
            CliMsg::Leader(status) => {
                self.leader = Some(status);
                match self.guard.kill_attempt() {
                    Ok(()) => self.guard.disarm(),
                    Err(error) if is_process_gone(&error) => self.guard.disarm(),
                    Err(error) => {
                        self.sweep_error = Some(format!("kill {}", error.kind()));
                    }
                }
                Ok(())
            }
            CliMsg::LeaderFailed(error) => {
                self.ended = true;
                Err(ProviderError::Transport {
                    family: Family::GoogleGeminiCli,
                    reason: format!("waiting for gemini failed: {error}"),
                })
            }
        }
    }

    /// Ends the input once the leader exited and every pipe drained: the decoder's verdict first,
    /// then the exit-status diagnosis when no `result` arrived. Stderr contributes a drained-byte
    /// count only, never its bytes.
    fn finish_terminal(&mut self) -> Result<(), ProviderError> {
        const FAMILY: Family = Family::GoogleGeminiCli;
        self.ended = true;
        if self.decoder.end().is_ok() {
            return Ok(());
        }
        let status = self
            .leader
            .or_else(|| self.guard.child.try_wait().ok().flatten());
        match status {
            Some(status) if !status.success() => {
                let base =
                    exit_failure(status.code()).unwrap_or_else(|| ProviderError::Transport {
                        family: FAMILY,
                        reason: if self.stderr_bytes == 0 {
                            format!("gemini exited with {status} and no result")
                        } else {
                            format!(
                                "gemini exited with {status} and no result ({} stderr bytes withheld)",
                                self.stderr_bytes
                            )
                        },
                    });
                if let Some(writer) = self.writer_error.take() {
                    Err(with_writer_cause(base, &writer))
                } else {
                    Err(base)
                }
            }
            Some(_) => {
                if let Some(error) = self.writer_error.take() {
                    return Err(ProviderError::Transport {
                        family: FAMILY,
                        reason: format!("writing the gemini prompt failed: {error}"),
                    });
                }
                Err(ProviderError::StreamCut)
            }
            None => Err(ProviderError::Transport {
                family: FAMILY,
                reason: String::from("gemini ended with no result"),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
