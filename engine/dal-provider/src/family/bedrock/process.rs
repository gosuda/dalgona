//! AWS `credential_process` runner, private to the Bedrock family.
//!
//! Splits one command line without a shell and runs the argv directly,
//! returning bounded stdout bytes for the caller to parse. Inputs are
//! borrowed; the env view is iterated once after `env_clear()`. The caller
//! owns the absolute resolution deadline; this module creates no timers
//! except `sleep_until(deadline)` and spawns no tasks. Errors carry only the
//! profile name, exit status, or I/O kind. Dropped futures only signal
//! termination; reaping happens on the explicit `.await` paths.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};

use dal_core::Family;
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::AsyncReadExt as _;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::error::{LimitError, ProviderError};
use crate::family::process::is_process_gone;
use crate::http::BODY_LIMIT;

const STDERR_SCRATCH_BYTES: usize = 8192;
const STDOUT_CHUNK_BYTES: usize = 8192;

const UNTERMINATED_QUOTE_MESSAGE: &str =
    "AWS credential_process command has an unterminated quote.";

fn unterminated_quote() -> ProviderError {
    ProviderError::InvalidRequest {
        message: String::from(UNTERMINATED_QUOTE_MESSAGE),
    }
}

fn empty_argv(profile: &str) -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!("AWS credential_process for profile '{profile}' is empty."),
    }
}

#[cfg(windows)]
fn batch_refusal(profile: &str) -> ProviderError {
    ProviderError::InvalidRequest {
        message: format!(
            "AWS profile '{profile}' names a batch script; \
             batch scripts require cmd.exe and no shell path is authorized"
        ),
    }
}

fn spawn_failed(profile: &str, error: &io::Error) -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: format!(
            "AWS credential_process for profile '{profile}' failed to start ({}).",
            error.kind()
        ),
    }
}

fn io_failed(profile: &str, error: &io::Error) -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: format!(
            "AWS credential_process for profile '{profile}' failed while reading output ({}).",
            error.kind()
        ),
    }
}

fn cancelled(profile: &str) -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: format!("AWS credential_process for profile '{profile}' was cancelled."),
    }
}

fn timed_out(profile: &str) -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: format!(
            "AWS credential_process for profile '{profile}' reached its resolution deadline."
        ),
    }
}

fn nonzero_exit(profile: &str, status: ExitStatus) -> ProviderError {
    ProviderError::Transport {
        family: Family::Bedrock,
        reason: format!(
            "AWS credential_process for profile '{profile}' exited with status {status}."
        ),
    }
}

/// POSIX-shell argv split.
///
/// Space, tab, newline, and carriage return delimit outside quotes. Single
/// quotes are literal; double quotes escape only `$`, `` ` ``, `"`, `\`.
/// A trailing backslash with no next character stays literal. Empty quotes
/// yield one empty token.
pub(crate) fn tokenize_posix(cmd: &str) -> Result<Vec<Box<str>>, ProviderError> {
    #[derive(Eq, PartialEq)]
    enum Mode {
        Normal,
        Single,
        Double,
    }

    let mut tokens: Vec<Box<str>> = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut mode = Mode::Normal;
    let mut chars = cmd.chars();

    while let Some(ch) = chars.next() {
        match mode {
            Mode::Single => {
                if ch == '\'' {
                    mode = Mode::Normal;
                } else {
                    current.push(ch);
                }
            }
            Mode::Double => {
                if ch == '"' {
                    mode = Mode::Normal;
                } else if ch == '\\' {
                    match chars.clone().next() {
                        Some(next) if next == '$' || next == '`' || next == '"' || next == '\\' => {
                            let _ = chars.next();
                            current.push(next);
                        }
                        _ => {
                            current.push(ch);
                        }
                    }
                } else {
                    current.push(ch);
                }
            }
            Mode::Normal => {
                if ch == '\'' {
                    mode = Mode::Single;
                    has_token = true;
                } else if ch == '"' {
                    mode = Mode::Double;
                    has_token = true;
                } else if ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    } else {
                        current.push(ch);
                    }
                    has_token = true;
                } else if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
                    if has_token {
                        tokens.push(current.into_boxed_str());
                        current = String::new();
                        has_token = false;
                    }
                } else {
                    current.push(ch);
                    has_token = true;
                }
            }
        }
    }

    if mode != Mode::Normal {
        return Err(unterminated_quote());
    }
    if has_token {
        tokens.push(current.into_boxed_str());
    }
    Ok(tokens)
}

/// Windows C-runtime argv split.
///
/// Only space and tab delimit outside double quotes. Backslash runs before a
/// quote follow `2N` / `2N+1` rules; all other backslashes are literal.
/// Newlines are token text, so paths survive intact.
#[cfg(any(windows, test))]
pub(crate) fn tokenize_windows(cmd: &str) -> Result<Vec<Box<str>>, ProviderError> {
    let mut tokens: Vec<Box<str>> = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut quoted = false;
    let mut backslashes: usize = 0;

    for ch in cmd.chars() {
        if ch == '\\' {
            backslashes = backslashes.saturating_add(1);
            continue;
        }
        if ch == '"' {
            for _ in 0..(backslashes >> 1) {
                current.push('\\');
            }
            has_token = true;
            let escaped = (backslashes & 1) == 1;
            backslashes = 0;
            if escaped {
                current.push('"');
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if backslashes > 0 {
            for _ in 0..backslashes {
                current.push('\\');
            }
            has_token = true;
            backslashes = 0;
        }
        if (ch == ' ' || ch == '\t') && !quoted {
            if has_token {
                tokens.push(current.into_boxed_str());
                current = String::new();
                has_token = false;
            }
            continue;
        }
        current.push(ch);
        has_token = true;
    }

    if quoted {
        return Err(unterminated_quote());
    }
    if backslashes > 0 {
        for _ in 0..backslashes {
            current.push('\\');
        }
        has_token = true;
    }
    if has_token {
        tokens.push(current.into_boxed_str());
    }
    Ok(tokens)
}

/// Splits with the platform grammar: Windows CRT on Windows, POSIX elsewhere.
pub(crate) fn tokenize_credential_process_command(
    cmd: &str,
) -> Result<Vec<Box<str>>, ProviderError> {
    #[cfg(windows)]
    {
        tokenize_windows(cmd)
    }
    #[cfg(not(windows))]
    {
        tokenize_posix(cmd)
    }
}

/// Splits and refuses an empty argv, naming only the profile.
pub(crate) fn split_credential_process_argv(
    cmd: &str,
    profile: &str,
) -> Result<Vec<Box<str>>, ProviderError> {
    let tokens = tokenize_credential_process_command(cmd)?;
    if tokens.is_empty() {
        return Err(empty_argv(profile));
    }
    Ok(tokens)
}

/// Whether `program` names a Windows batch script (`.cmd` / `.bat`).
#[must_use]
pub(crate) fn is_windows_batch_script(program: &str) -> bool {
    std::path::Path::new(program)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// One direct-execution request. All references are borrowed.
///
/// `env` is the launch view only, applied once after `env_clear()`.
pub(crate) struct CredentialProcessRequest<'a> {
    /// First argv token, spawned directly, never via a shell.
    pub program: &'a str,
    /// Remaining argv tokens.
    pub args: &'a [Box<str>],
    /// Edge-captured working directory.
    pub cwd: &'a Path,
    /// Borrowed `(name, value)` view applied after `env_clear()`.
    pub env: &'a [(&'a str, &'a str)],
    /// Profile name for secret-free error text.
    pub profile: &'a str,
}

impl fmt::Debug for CredentialProcessRequest<'_> {
    /// Redacted: program, argv values, env values, and cwd never appear.
    /// Arg count, env names, and profile only.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env_names: Vec<&str> = self.env.iter().map(|(name, _)| *name).collect();
        f.debug_struct("CredentialProcessRequest")
            .field("args_len", &self.args.len())
            .field("env_names", &env_names)
            .field("profile", &self.profile)
            .finish()
    }
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

    /// Reaps for error paths. Disarms only on confirmed cleanup (reaped or
    /// already gone); other failures stay armed so Drop retries the signal.
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

    /// Reaps for the success path, returning the status. Already-gone maps to
    /// the observed leader status without claiming a second reap.
    async fn reap_for_status(&mut self, fallback: ExitStatus) -> io::Result<ExitStatus> {
        match self.child.wait().await {
            Ok(status) => {
                self.disarm();
                Ok(status)
            }
            Err(error) if is_process_gone(&error) => {
                self.disarm();
                Ok(fallback)
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

enum Round {
    Cancelled,
    Deadline,
    Leader(io::Result<ExitStatus>),
    Stdout(io::Result<usize>),
    Stderr(io::Result<usize>),
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

/// Single kill-and-reap invariant for every error path.
///
/// Best-effort termination, then reap. An already-gone child or group
/// (`is_process_gone`) is benign. Other cleanup failures are chained onto the
/// original error without subprocess output. Disarms only on confirmed
/// cleanup. Unbounded: after the kill signal death is certain except for
/// uninterruptible stalls, and the resolution deadline may already have
/// fired, so no second bound is claimed.
async fn cleanup_on_error(guard: &mut ChildGuard, original: ProviderError) -> ProviderError {
    let kill_cleanup: Option<String> = match guard.kill_attempt() {
        Ok(()) => None,
        Err(error) if is_process_gone(&error) => None,
        Err(error) => Some(format!("kill {}", error.kind())),
    };
    if let Err(error) = guard.reap_attempt().await {
        let reap = format!("reap {}", error.kind());
        let cleanup = match kill_cleanup {
            Some(kind) => format!("{kind}; {reap}"),
            None => reap,
        };
        return combine_with_cleanup(original, &cleanup);
    }
    match kill_cleanup {
        Some(kind) => combine_with_cleanup(original, &kind),
        None => original,
    }
}

/// Runs one helper directly and returns its bounded stdout bytes.
///
/// Uses the caller's absolute deadline via `sleep_until` only. The leader is
/// observed through the inner direct child; the group/job is terminated
/// before pipes drain, then reaped. Never spawns, never shells, never reads
/// ambient state.
#[expect(
    clippy::too_many_lines,
    reason = "credential process execution manages child process, stdin, stdout, stderr, timeout, and cancellation"
)]
pub(crate) async fn run_credential_process(
    req: &CredentialProcessRequest<'_>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<Box<[u8]>, ProviderError> {
    if req.program.is_empty() {
        return Err(empty_argv(req.profile));
    }
    #[cfg(windows)]
    if is_windows_batch_script(req.program) {
        return Err(batch_refusal(req.profile));
    }
    if cancel.is_cancelled() {
        return Err(cancelled(req.profile));
    }
    if Instant::now() >= deadline {
        return Err(timed_out(req.profile));
    }

    let mut command = CommandWrap::with_new(req.program, |process| {
        process.args(req.args.iter().map(Box::as_ref));
        process.current_dir(req.cwd);
        process.env_clear();
        for (name, value) in req.env {
            process.env(*name, *value);
        }
        process.stdin(Stdio::null());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
    });
    #[cfg(unix)]
    {
        command.wrap(ProcessGroup::leader());
        command.wrap(KillOnDrop);
    }
    #[cfg(windows)]
    {
        command.wrap(JobObject);
        command.wrap(KillOnDrop);
    }

    let child = command
        .spawn()
        .map_err(|error| spawn_failed(req.profile, &error))?;
    let mut guard = ChildGuard::new(child);
    let mut stdout = guard.child.stdout().take();
    let mut stderr = guard.child.stderr().take();

    let mut out: Vec<u8> = Vec::new();
    let mut stdout_done = stdout.is_none();
    let mut stderr_done = stderr.is_none();
    let mut leader: Option<ExitStatus> = None;
    let mut stdout_scratch = [0_u8; STDOUT_CHUNK_BYTES];
    let mut stderr_scratch = [0_u8; STDERR_SCRATCH_BYTES];

    loop {
        if leader.is_some() && stdout_done && stderr_done {
            break;
        }
        let round = tokio::select! {
            biased;
            () = cancel.cancelled() => Round::Cancelled,
            () = sleep_until(deadline) => Round::Deadline,
            status = async {
                guard.child.inner_mut().wait().await
            }, if leader.is_none() => Round::Leader(status),
            count = async {
                match stdout.as_mut() {
                    Some(pipe) => pipe.read(&mut stdout_scratch).await,
                    None => std::future::pending::<io::Result<usize>>().await,
                }
            }, if !stdout_done => Round::Stdout(count),
            count = async {
                match stderr.as_mut() {
                    Some(pipe) => pipe.read(&mut stderr_scratch).await,
                    None => std::future::pending::<io::Result<usize>>().await,
                }
            }, if !stderr_done => Round::Stderr(count),
        };
        match round {
            Round::Cancelled => {
                return Err(cleanup_on_error(&mut guard, cancelled(req.profile)).await);
            }
            Round::Deadline => {
                return Err(cleanup_on_error(&mut guard, timed_out(req.profile)).await);
            }
            Round::Leader(Ok(status)) => {
                leader = Some(status);
                match guard.kill_attempt() {
                    Ok(()) => {}
                    Err(error) if is_process_gone(&error) => {}
                    Err(error) => {
                        return Err(
                            cleanup_on_error(&mut guard, io_failed(req.profile, &error)).await
                        );
                    }
                }
            }
            Round::Leader(Err(error)) | Round::Stdout(Err(error)) | Round::Stderr(Err(error)) => {
                return Err(cleanup_on_error(&mut guard, io_failed(req.profile, &error)).await);
            }
            Round::Stdout(Ok(0)) => {
                stdout_done = true;
            }
            Round::Stdout(Ok(count)) => {
                if out.len().saturating_add(count) > BODY_LIMIT {
                    return Err(cleanup_on_error(
                        &mut guard,
                        ProviderError::Limit(LimitError::Body),
                    )
                    .await);
                }
                out.extend_from_slice(&stdout_scratch[..count]);
            }
            Round::Stderr(Ok(0)) => {
                stderr_done = true;
            }
            Round::Stderr(Ok(_)) => {}
        }
    }

    let Some(observed) = leader else {
        unreachable!("capture loop breaks only after the leader is observed");
    };
    let status = match guard.reap_for_status(observed).await {
        Ok(status) => status,
        Err(error) => return Err(io_failed(req.profile, &error)),
    };
    if !status.success() {
        return Err(nonzero_exit(req.profile, status));
    }
    Ok(out.into_boxed_slice())
}

#[cfg(test)]
mod tests;
