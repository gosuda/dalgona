//! Shell execution tool contracts, state mapping, and user-facing results.

/// Tool-specific classification of parsed exec commands.
pub mod classify;
/// Shell resolution against configured paths and platform ladders.
pub(crate) mod shell;

pub use classify::exec_reads_only;

use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use dal_agent::{
    FULL_OUTPUT_PREFIX, ProcResult, ProcStatus, SpawnOpts, ToolError,
    ext::{ArgError, BoxFuture, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput},
};
use dal_core::{
    CallId, ModelInfo, Name, Preview, RawJson, RegistrationError, ToolClass, ToolSpec, Workspace,
};
use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};

/// Configuration for the `exec` tool.
#[derive(Clone, Debug)]
pub struct ExecConfig {
    /// A configured shell executable path. When set, the call fails if it is not executable.
    pub shell: Option<String>,
    /// Foreground time in seconds before the process becomes a detached job.
    pub foreground_seconds: u64,
    /// Whether to append the sandbox-permission diagnostic note to output.
    pub sandbox_on: bool,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            shell: None,
            foreground_seconds: 60,
            sandbox_on: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExecArgs {
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_seconds: Option<i64>,
    pub foreground_s: Option<i64>,
}

/// A command argument, cwd, timeout, shell, or process start failed validation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ExecError {
    /// An argument field is not part of the exec schema.
    #[error("exec: unknown argument \"{field}\"")]
    UnknownField {
        /// The rejected field name.
        field: String,
    },
    /// An argument field has the wrong JSON type.
    #[error("exec: {field} must be {want}")]
    FieldType {
        /// The field with the wrong type.
        field: &'static str,
        /// The required JSON type.
        want: &'static str,
    },
    /// The argument document is not an object.
    #[error("exec: command must be a string")]
    NotAnObject,
    /// The command is empty or contains only whitespace.
    #[error("exec: command must not be empty")]
    EmptyCommand,
    /// The timeout is outside the accepted range.
    #[error("exec: timeout_seconds must be between 1 and 86400")]
    TimeoutRange,
    /// The per-call foreground window is outside the accepted range.
    #[error("exec: foreground_s must be between 1 and 86400")]
    ForegroundRange,
    /// The cwd is absolute or resolves outside the workspace root.
    #[error("exec: cwd must be relative to the workspace root")]
    CwdNotRelative,
    /// The requested cwd does not resolve to an existing path.
    #[error("exec: cwd {0} does not exist")]
    CwdMissing(String),
    /// The configured shell does not exist or is not executable.
    #[error("exec: shell {path} does not exist")]
    ShellMissing {
        /// The configured shell path.
        path: String,
    },
    /// No Windows Git Bash executable was found.
    #[cfg(windows)]
    #[error(
        "exec: no bash found. Install Git for Windows (https://git-scm.com/downloads/win), add bash.exe to PATH, or set shell in dal.toml"
    )]
    NoBash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedCall {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Option<Duration>,
    pub timeout_seconds: Option<u64>,
    pub foreground: Option<Duration>,
}

/// Decodes one exec argument object while preserving the byte-stable error literals.
pub(crate) fn decode(text: &str) -> Result<ExecArgs, ExecError> {
    let value: sonic_rs::Value = sonic_rs::from_str(text).map_err(|_| ExecError::NotAnObject)?;
    let object = value.as_object().ok_or(ExecError::NotAnObject)?;
    let mut command = None;
    let mut cwd = None;
    let mut timeout_seconds = None;
    let mut foreground_s = None;

    for (key, value) in object {
        match key {
            "command" => {
                command = Some(
                    value
                        .as_str()
                        .ok_or(ExecError::FieldType {
                            field: "command",
                            want: "a string",
                        })?
                        .to_owned(),
                );
            }
            "cwd" => {
                cwd = Some(
                    value
                        .as_str()
                        .ok_or(ExecError::FieldType {
                            field: "cwd",
                            want: "a string",
                        })?
                        .to_owned(),
                );
            }
            "timeout_seconds" => {
                timeout_seconds = Some(value.as_i64().ok_or(ExecError::FieldType {
                    field: "timeout_seconds",
                    want: "an integer",
                })?);
            }
            "foreground_s" => {
                foreground_s = Some(value.as_i64().ok_or(ExecError::FieldType {
                    field: "foreground_s",
                    want: "an integer",
                })?);
            }
            other => {
                return Err(ExecError::UnknownField {
                    field: other.into(),
                });
            }
        }
    }

    Ok(ExecArgs {
        command: command.ok_or(ExecError::FieldType {
            field: "command",
            want: "a string",
        })?,
        cwd,
        timeout_seconds,
        foreground_s,
    })
}

/// Maps a per-call `foreground_s` to its window, inside the timeout range.
fn foreground_window(seconds: i64) -> Result<Duration, ExecError> {
    u64::try_from(seconds)
        .ok()
        .filter(|seconds| (1..=86_400).contains(seconds))
        .map(Duration::from_secs)
        .ok_or(ExecError::ForegroundRange)
}

/// Resolves the cwd and timeout before an approval request or process spawn.
pub(crate) fn validate(args: ExecArgs, root: &Path) -> Result<ValidatedCall, ExecError> {
    if args.command.trim().is_empty() {
        return Err(ExecError::EmptyCommand);
    }

    let timeout_seconds = args
        .timeout_seconds
        .map(|seconds| u64::try_from(seconds).map_err(|_| ExecError::TimeoutRange))
        .transpose()?;
    if timeout_seconds.is_some_and(|seconds| !(1..=86_400).contains(&seconds)) {
        return Err(ExecError::TimeoutRange);
    }
    let timeout = timeout_seconds.map(Duration::from_secs);
    let foreground = args.foreground_s.map(foreground_window).transpose()?;
    let cwd = match args.cwd.as_deref() {
        None | Some("") => root.to_path_buf(),
        Some(path) => {
            let relative = Path::new(path);
            if relative.is_absolute() {
                return Err(ExecError::CwdNotRelative);
            }
            let joined = root.join(relative);
            let canonical = std::fs::canonicalize(&joined)
                .map_err(|_| ExecError::CwdMissing(path.to_owned()))?;
            let canonical_root =
                std::fs::canonicalize(root).map_err(|_| ExecError::CwdNotRelative)?;
            if !canonical.starts_with(&canonical_root) {
                return Err(ExecError::CwdNotRelative);
            }
            canonical
        }
    };

    Ok(ValidatedCall {
        command: args.command,
        cwd,
        timeout,
        timeout_seconds,
        foreground,
    })
}

/// Process-door lifecycle stage of one exec job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecState {
    /// Created; waiting for the host to start the shell process.
    Queued,
    /// The shell process is running under a deadline.
    Running,
    /// The foreground budget elapsed; the job continues in the background.
    Detached,
    /// The host is stopping the process group of the job.
    LadderPending,
    /// The job produced its user-facing result.
    Done,
}

/// Terminal condition of one exec job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecOutcome {
    /// The command exited with the given status code.
    Exited(i32),
    /// The command was killed by the given signal.
    Signaled(i32),
    /// The deadline elapsed before the command exited.
    TimedOut,
    /// The turn ended before the command exited.
    Aborted,
}

/// Host door event applied to one exec job by [`ExecJob::on`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecEvent {
    /// The host started the shell process at `at`.
    Spawned {
        /// The instant the host door reported the process start.
        at: Instant,
    },
    /// The process exited or was stopped with the given outcome.
    Exit(ExecOutcome),
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "state-machine inputs the host driver does not yet emit; exercised by exec/tests.rs"
        )
    )]
    TimeoutFire,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "state-machine inputs the host driver does not yet emit; exercised by exec/tests.rs"
        )
    )]
    Cancel,
    BudgetFire,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "state-machine inputs the host driver does not yet emit; exercised by exec/tests.rs"
        )
    )]
    LadderComplete(ExecOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecTransition {
    StartTimeout,
    EmitResult(ExecOutcome),
    NoticeAndDone(ExecOutcome),
    Ladder(ExecOutcome),
    FinishAfterLadder(ExecOutcome),
    Detach,
}

#[derive(Debug)]
pub(crate) struct ExecJob {
    state: ExecState,
    call_id: CallId,
    command: String,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
}

impl ExecJob {
    pub(crate) fn new(call_id: CallId, command: String, timeout: Option<Duration>) -> Self {
        Self {
            state: ExecState::Queued,
            call_id,
            command,
            timeout,
            deadline: None,
        }
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read by exec/tests.rs; the host driver reads the deadline internally"
        )
    )]
    #[must_use]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Applies one process-door event to the exec-side job state. Calling this
    /// with an event the current state does not accept returns `None`; the
    /// illegal pair names itself in `debug_assertions` builds only.
    pub(crate) fn on(&mut self, event: ExecEvent) -> Option<ExecTransition> {
        use ExecState::{Detached, LadderPending, Queued, Running};

        match (self.state, event) {
            (Queued, ExecEvent::Spawned { at }) => {
                self.state = Running;
                self.deadline = self.timeout.map(|timeout| at + timeout);
                Some(ExecTransition::StartTimeout)
            }
            (Queued, ExecEvent::Cancel) => {
                self.state = ExecState::Done;
                Some(ExecTransition::EmitResult(ExecOutcome::Aborted))
            }
            (Running, ExecEvent::Exit(outcome)) => {
                self.state = ExecState::Done;
                Some(ExecTransition::EmitResult(outcome))
            }
            (Running, ExecEvent::TimeoutFire) => {
                self.state = LadderPending;
                Some(ExecTransition::Ladder(ExecOutcome::TimedOut))
            }
            (Running | Detached, ExecEvent::Cancel) => {
                self.state = LadderPending;
                Some(ExecTransition::Ladder(ExecOutcome::Aborted))
            }
            (Running, ExecEvent::BudgetFire) => {
                self.state = Detached;
                Some(ExecTransition::Detach)
            }
            (Detached, ExecEvent::Exit(outcome)) => {
                self.state = ExecState::Done;
                Some(ExecTransition::NoticeAndDone(outcome))
            }
            (LadderPending, ExecEvent::LadderComplete(outcome)) => {
                self.state = ExecState::Done;
                Some(ExecTransition::FinishAfterLadder(outcome))
            }
            (state, event) => {
                debug_assert!(
                    false,
                    "illegal exec event {event:?} in state {state:?} for call {} ({})",
                    self.call_id.as_str(),
                    self.command
                );
                None
            }
        }
    }
}

/// Formats the bounded preview, durable output path, exit state, and optional sandbox note.
pub(crate) fn final_text(
    preview: Option<&str>,
    log_path: &Path,
    outcome: ExecOutcome,
    timeout_seconds: Option<u64>,
    sandbox_on: bool,
    denial_seen: bool,
) -> String {
    let mut output = String::new();
    if let Some(preview) = preview.filter(|preview| !preview.is_empty()) {
        output.push_str(preview);
        output.push('\n');
    }
    output.push_str(FULL_OUTPUT_PREFIX);
    output.push_str(&log_path.to_string_lossy());
    match outcome {
        ExecOutcome::Exited(0) => {}
        ExecOutcome::Exited(code) => {
            output.push_str("\nCommand exited with code ");
            let _ = write!(output, "{code}");
        }
        ExecOutcome::Signaled(signal) => {
            output.push_str("\nCommand was killed by signal ");
            let _ = write!(output, "{signal}");
        }
        ExecOutcome::TimedOut => match timeout_seconds {
            Some(seconds) => {
                output.push_str("\nCommand timed out after ");
                let _ = write!(output, "{seconds}");
                output.push_str(" seconds");
            }
            None => output.push_str("\nCommand timed out"),
        },
        ExecOutcome::Aborted => output.push_str("\nCommand aborted"),
    }
    if sandbox_on && denial_seen {
        output.push_str("\ndalgon sandbox: a \"Permission denied\" or \"Operation not permitted\" error can come from the sandbox; if the path should be writable, add it to sandbox_writable in dal.toml.");
    }
    output
}

/// Schema bytes for `Tool::spec`. Single source of the field list.
pub(crate) const EXEC_SCHEMA: &str = "{\"type\":\"object\",\"properties\":{\"command\":{\"type\":\"string\",\"description\":\"Shell command to run. A read-only command may run beside parallel reads.\"},\"cwd\":{\"type\":\"string\",\"description\":\"Working directory, relative to the workspace root. Default: the workspace root.\"},\"timeout_seconds\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":86400,\"description\":\"Seconds before dalgon stops the command. Default: no timeout.\"},\"foreground_s\":{\"type\":\"integer\",\"minimum\":1,\"maximum\":86400,\"description\":\"Seconds this call waits before the command continues as a background job. Default: the exec.foreground_seconds setting.\"}},\"required\":[\"command\"],\"additionalProperties\":false}";

/// POSIX description. Windows replaces the two process-group sentences with:
/// `On timeout, cancel, or exit, dalgon stops the job object of the command and every process in it.`
pub(crate) const EXEC_DESCRIPTION: &str = "Run one shell command and return its output. The command runs with <shell> -c; stdin is closed. The environment adds NO_COLOR=1, TERM=dumb, PAGER=cat, and DAL_NESTED=1. Output shows the last 4096 bytes, and a line names the file that holds the full output. timeout_seconds bounds the run in seconds; without it the command runs until it exits or the turn is cancelled. foreground_s sets how many seconds the call waits before the command continues as a background job; without it the call waits for the exec.foreground_seconds setting. On timeout or cancel, dalgon sends SIGTERM to the process group of the command and SIGKILL 2 seconds later. When the command exits, dalgon stops processes that it left running in its process group. A non-zero exit code makes the result an error. exec is not a sandbox. A read-only command joins parallel reads; other commands run one at a time and ask for approval as configured.";

pub(crate) struct ExecTool {
    cfg: ExecConfig,
    name: Name,
    spec: Arc<ToolSpec>,
}

impl ExecTool {
    fn new(cfg: ExecConfig) -> Result<Self, RegistrationError> {
        let name = Name::parse("exec")?;
        let parameters =
            RawJson::parse(EXEC_SCHEMA).map_err(|_| RegistrationError::InvalidParameters)?;
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: EXEC_DESCRIPTION.into(),
            parameters,
            grammar: None,
        });
        Ok(Self { cfg, name, spec })
    }

    async fn drive<'a>(&'a self, call: ToolCall, mut cx: ToolCx<'a>) -> ToolOutcome {
        let parsed = match decode(call.args.as_str()) {
            Ok(parsed) => parsed,
            Err(error) => return ToolOutcome::Err(ToolError::message(error.to_string())),
        };
        let root = cx.workspace().as_path().to_path_buf();
        let shell_setting = self.cfg.shell.clone();
        let sandbox_on = self.cfg.sandbox_on;
        let foreground_seconds = self.cfg.foreground_seconds;
        let captured_vars = cx.env().vars.clone();
        let blocking = tokio::task::spawn_blocking(move || {
            let call = validate(parsed, &root)?;
            let shell = shell::resolve(shell_setting.as_deref(), &call.cwd, &captured_vars)?;
            Ok::<_, ExecError>((call, shell))
        })
        .await;
        let (validated, shell) = match blocking {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => return ToolOutcome::Err(ToolError::message(error.to_string())),
            Err(join) => {
                return ToolOutcome::Err(ToolError::message(format!(
                    "exec: blocking task failed: {join}"
                )));
            }
        };
        let preview = Preview {
            title: "exec".into(),
            body: validated.command.clone().into(),
            digest: None,
        };
        let approved = match cx.authorize(preview).await {
            Ok(approved) => approved,
            Err(reason) => return ToolOutcome::Err(ToolError::Denied(reason)),
        };
        let argv = [
            OsString::from(shell.program.as_os_str()),
            OsString::from("-c"),
            OsString::from(&validated.command),
        ];
        let opts = SpawnOpts {
            cwd: validated.cwd.clone(),
            timeout: validated.timeout,
            env: Vec::new(),
        };
        let mut proc = match cx.spawn(&argv, opts, approved) {
            Ok(proc) => proc,
            Err(ToolError::Spawn { source, .. }) => {
                return ToolOutcome::Err(ToolError::message(format!(
                    "exec: cannot start {}: {source}",
                    shell.program.display()
                )));
            }
            Err(error) => return ToolOutcome::Err(error),
        };
        let mut job = ExecJob::new(
            call.id.clone(),
            validated.command.clone(),
            validated.timeout,
        );
        job.on(ExecEvent::Spawned { at: Instant::now() });
        let budget = validated
            .foreground
            .unwrap_or(Duration::from_secs(foreground_seconds));
        let waited = tokio::select! {
            biased;
            waited = proc.wait(cx.cancel()) => waited,
            () = tokio::time::sleep(budget) => {
                job.on(ExecEvent::BudgetFire);
                return ToolOutcome::Detached(cx.detach(proc));
            }
        };
        Self::settle(&mut job, &validated, waited, sandbox_on)
    }

    /// Maps one finished process result to the user-facing exec outcome.
    fn settle(
        job: &mut ExecJob,
        validated: &ValidatedCall,
        result: Result<ProcResult, ToolError>,
        sandbox_on: bool,
    ) -> ToolOutcome {
        let result = match result {
            Ok(result) => result,
            Err(error) => return ToolOutcome::Err(error),
        };
        let outcome = match result.status {
            ProcStatus::Exited { code } => ExecOutcome::Exited(code),
            ProcStatus::Signaled { signal } => ExecOutcome::Signaled(signal),
            ProcStatus::TimedOut => ExecOutcome::TimedOut,
            ProcStatus::Cancelled => ExecOutcome::Aborted,
        };
        job.on(ExecEvent::Exit(outcome));
        let preview_text = String::from_utf8_lossy(&result.preview);
        let preview = (!result.preview.is_empty()).then_some(preview_text.as_ref());
        let text = final_text(
            preview,
            &result.log_path,
            outcome,
            validated.timeout_seconds,
            sandbox_on,
            result.denial_seen,
        );
        match outcome {
            ExecOutcome::Exited(0) => ToolOutcome::Ok(ToolOutput::from_text(text.into_boxed_str())),
            _ => ToolOutcome::Err(ToolError::message(text)),
        }
    }
}

impl Tool for ExecTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        let parsed = decode(args.as_str()).map_err(|error| ArgError::message(error.to_string()))?;
        Ok(ToolClass::Exec {
            read_only: exec_reads_only(&parsed.command, parsed.cwd.as_deref()),
            grant: None,
        })
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(self.drive(call, cx))
    }
}

/// Registers the exec tool, visibility `model`, no hooks, no schemes, no
/// capabilities. The read and search tools part's `extension()` calls this
/// with `cfg.exec`; exec never edits that file.
pub(crate) fn exec_extension(cfg: ExecConfig) -> Result<Arc<dyn Tool>, RegistrationError> {
    Ok(Arc::new(ExecTool::new(cfg)?))
}
#[cfg(test)]
mod tests;
