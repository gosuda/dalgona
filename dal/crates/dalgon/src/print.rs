//! Headless prompt streaming for text and JSON clients.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Agent, AgentError, Delivery};
use dal_core::{
    CancelScope, Command, Expect, Part, Reply, Stop, StreamChannel, UpdateKind,
    parse_headless_denial,
};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub(crate) const QUIET_WAIT: Duration = Duration::from_secs(3);

/// Pause between quiet checks while an extension is still busy.
const QUIET_RECHECK: Duration = Duration::from_millis(50);

/// A prompt positional that the edge could not turn into message content.
#[derive(Debug, Error)]
pub(crate) enum PromptError {
    /// No prompt was given and standard input is a terminal.
    #[error("empty")]
    Empty,
    /// `-` was given while standard input is a terminal.
    #[error("piped")]
    DashWithoutPipe,
    /// An `@FILE` path does not exist under the workspace.
    #[error("missing")]
    FileMissing { path: PathBuf, arg: String },
    /// An `@FILE` path could not be read.
    #[error("unreadable")]
    FileUnreadable { path: PathBuf, source: io::Error },
    /// An `@FILE` path is not UTF-8 text or a known image.
    #[error("encoding")]
    FileNotUtf8 { path: PathBuf },
    /// Piped standard input could not be read.
    #[error("stdin")]
    Stdin(#[source] io::Error),
}

/// Builds first-message content from prompt positionals against the workspace.
///
/// `@FILE` resolves under `workspace`; `-` consumes piped standard input once
/// no matter how often it appears. With no positionals, piped standard input
/// becomes the prompt; a terminal standard input reports [`PromptError::Empty`].
pub(crate) async fn assemble_prompt(
    prompts: &[OsString],
    workspace: &Path,
    stdin_tty: bool,
) -> Result<Vec<Part>, PromptError> {
    let mut stdin_cache: Option<String> = None;
    if prompts.is_empty() {
        if stdin_tty {
            return Err(PromptError::Empty);
        }
        return Ok(vec![Part::Text {
            text: read_stdin_once(&mut stdin_cache).await?.into(),
        }]);
    }
    let mut parts = Vec::with_capacity(prompts.len());
    for prompt in prompts {
        if prompt.as_os_str() == OsStr::new("-") {
            if stdin_tty {
                return Err(PromptError::DashWithoutPipe);
            }
            parts.push(Part::Text {
                text: read_stdin_once(&mut stdin_cache).await?.into(),
            });
        } else if let Some(file) = prompt
            .to_str()
            .filter(|text| text.starts_with('@'))
            .map(|text| text.trim_start_matches('@'))
        {
            parts.push(read_prompt_file(workspace, file, prompt).await?);
        } else {
            parts.push(Part::Text {
                text: prompt.to_string_lossy().into_owned().into(),
            });
        }
    }
    Ok(parts)
}

#[expect(
    clippy::disallowed_methods,
    reason = "R4 edge: print mode reads prompt text from standard input"
)]
async fn read_stdin_once(cache: &mut Option<String>) -> Result<String, PromptError> {
    if let Some(text) = cache {
        return Ok(text.clone());
    }
    let mut text = String::new();
    tokio::io::stdin()
        .read_to_string(&mut text)
        .await
        .map_err(PromptError::Stdin)?;
    *cache = Some(text.clone());
    Ok(text)
}

async fn read_prompt_file(
    workspace: &Path,
    file: &str,
    prompt: &OsString,
) -> Result<Part, PromptError> {
    let relative = Path::new(file);
    let path = workspace.join(relative);
    let bytes = tokio::fs::read(&path).await.map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            PromptError::FileMissing {
                path: path.clone(),
                arg: prompt.to_string_lossy().into_owned(),
            }
        } else {
            PromptError::FileUnreadable {
                path: path.clone(),
                source,
            }
        }
    })?;
    if let Some(mime) = image_mime(relative) {
        return Ok(Part::Image {
            mime: mime.into(),
            bytes: bytes.into(),
        });
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Part::Text { text: text.into() }),
        Err(_) => Err(PromptError::FileNotUtf8 { path }),
    }
}

fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct PrintOptions {
    pub json: bool,
    pub output_last_message: Option<PathBuf>,
    pub prompt: Vec<dal_core::Part>,
    pub stderr_is_tty: bool,
    /// Approval mode `ask` with no terminal: the run emits the once-per-run
    /// headless notice before the first denial note.
    pub headless_approval: bool,
    pub stop: CancellationToken,
    pub quiet_wait: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrintOutcome {
    Completed,
    /// The run completed but one or more gated calls were denied.
    Denied,
    Interrupted,
}

#[derive(Debug, Error)]
pub(crate) enum PrintError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("could not write output: {0}")]
    Io(#[source] std::io::Error),
    #[error("could not write last assistant message to {path}: {source}")]
    OutputFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the session returned an unexpected prompt reply")]
    UnexpectedReply,
    #[error("the session ended its update stream before the turn ended")]
    SubscriptionClosed,
    #[error("the session update stream requires a resync during print")]
    UpdatesLost,
    #[error("the model turn failed without an error notice")]
    FailedWithoutMessage,
    #[error("{0}")]
    TurnFailed(Box<str>),
    #[error("extension status is not quiet: {busy} still busy")]
    NotQuiet { busy: Box<str>, wait: Duration },
}

impl PrintError {
    pub(crate) fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::BrokenPipe)
    }

    pub(crate) fn diagnostic(&self) -> (String, String) {
        match self {
            Self::Agent(error) => (
                error.to_string(),
                "Check the session and provider configuration, then try again.".to_owned(),
            ),
            Self::Io(error) => (
                format!("could not write output: {error}"),
                "Check that the output stream is open, then try again.".to_owned(),
            ),
            Self::OutputFile { path, source } => (
                format!("dalgon: cannot write {}: {source}", path.display()),
                format!(
                    "Fix the permissions on {}, or point the XDG variables at a writable location.",
                    path.display()
                ),
            ),
            Self::UnexpectedReply => {
                let [what, hint] = dal_texts::internal_error(
                    "print",
                    "the session returned an unexpected prompt reply",
                );
                (what, hint)
            }
            Self::SubscriptionClosed => {
                let [what, hint] = dal_texts::internal_error(
                    "print",
                    "the session ended its update stream before the turn ended",
                );
                (what, hint)
            }
            Self::UpdatesLost => {
                let [what, hint] = dal_texts::internal_error(
                    "print",
                    "the session update stream requires a resync",
                );
                (what, hint)
            }
            Self::FailedWithoutMessage => {
                let [what, hint] = dal_texts::internal_error(
                    "print",
                    "the model turn failed without an error notice",
                );
                (what, hint)
            }
            Self::TurnFailed(message) => (
                message.to_string(),
                "Check the provider response and try the prompt again.".to_owned(),
            ),
            Self::NotQuiet { busy, wait } => {
                let [what, hint] = dal_texts::status_not_quiet_timeout(busy, wait.as_secs());
                (what, hint)
            }
        }
    }
}

pub(crate) async fn run_print(
    agent: Agent,
    opts: PrintOptions,
    stdout: &mut (impl AsyncWrite + Unpin),
    stderr: &mut (impl AsyncWrite + Unpin),
) -> Result<PrintOutcome, PrintError> {
    let json = opts.json;
    match run_print_inner(agent, opts, stdout, stderr).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if !error.is_broken_pipe() {
                write_failure(&error, json, stdout, stderr).await?;
            }
            Err(error)
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one print run walks every output mode in place"
)]
async fn run_print_inner(
    agent: Agent,
    opts: PrintOptions,
    stdout: &mut (impl AsyncWrite + Unpin),
    stderr: &mut (impl AsyncWrite + Unpin),
) -> Result<PrintOutcome, PrintError> {
    if opts.stop.is_cancelled() {
        stdout.flush().await.map_err(PrintError::Io)?;
        return Ok(PrintOutcome::Interrupted);
    }
    // Listen-only: print mode can never answer an approval request, so it
    // must not count as an attached answerer. Only-answerer bookkeeping turns
    // asks into the spec's headless denial instead of a broker timeout.
    let mut subscription = agent.subscribe_listen(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: opts.prompt,
        })
        .await?;
    let Reply::Accepted { turn, .. } = reply else {
        return Err(PrintError::UnexpectedReply);
    };

    // The once-per-run headless notice precedes any per-call denial note.
    if opts.headless_approval && !opts.json {
        stderr
            .write_all(dal_texts::HEADLESS_APPROVAL.as_bytes())
            .await
            .map_err(PrintError::Io)?;
        stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
    }

    let mut assistant = String::new();
    let mut last_notice = None;
    let mut denial_count = 0_usize;
    let stop = loop {
        let delivery = tokio::select! {
            biased;
            () = opts.stop.cancelled() => {
                let _ = agent
                    .submit(Command::Cancel {
                        scope: CancelScope::Turn(turn),
                    })
                    .await;
                stdout.flush().await.map_err(PrintError::Io)?;
                return Ok(PrintOutcome::Interrupted);
            }
            delivery = subscription.next() => delivery,
        };
        let update = match delivery {
            Some(Delivery::Update(update)) => update,
            Some(Delivery::Resync { .. }) => return Err(PrintError::UpdatesLost),
            None => return Err(PrintError::SubscriptionClosed),
        };
        match &update.kind {
            UpdateKind::Delta {
                channel: StreamChannel::Text,
                text,
                ..
            } => {
                assistant.push_str(text);
                if !opts.json {
                    stdout
                        .write_all(text.as_bytes())
                        .await
                        .map_err(PrintError::Io)?;
                }
            }
            UpdateKind::ToolStarted { tool, .. } if opts.stderr_is_tty && !opts.json => {
                let progress = format!("Running {tool}.\n");
                stderr
                    .write_all(progress.as_bytes())
                    .await
                    .map_err(PrintError::Io)?;
            }
            UpdateKind::ToolSettled { outcome, .. } if !opts.json => {
                let Some((tool, rung)) = parse_headless_denial(&outcome.text) else {
                    continue;
                };
                let note = dal_texts::approval_denied_note(tool, rung.as_str());
                stderr
                    .write_all(note.as_bytes())
                    .await
                    .map_err(PrintError::Io)?;
                stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
                denial_count += 1;
            }
            UpdateKind::Notice(notice) => {
                last_notice = Some(notice.text.clone());
                if !opts.json {
                    stderr
                        .write_all(notice.text.as_bytes())
                        .await
                        .map_err(PrintError::Io)?;
                    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
                }
            }
            UpdateKind::TurnEnded { stop, .. } => break *stop,
            _ => {}
        }
    };
    drop(subscription);

    let quiet_end = match await_quiet(&agent, &opts.stop, opts.quiet_wait).await? {
        QuietEnd::Quiet => None,
        QuietEnd::Stopped => {
            stdout.flush().await.map_err(PrintError::Io)?;
            return Ok(PrintOutcome::Interrupted);
        }
        QuietEnd::TimedOut(busy) => Some(busy),
    };

    if stop == Stop::Failed {
        return Err(last_notice.map_or(PrintError::FailedWithoutMessage, PrintError::TurnFailed));
    }

    // One exact note per denied call; no summary sentence. A denied run is a
    // completed run with nothing to show: exit non-zero.
    if denial_count > 0 && !opts.json {
        return Ok(PrintOutcome::Denied);
    }

    if let Some(path) = opts.output_last_message
        && let Err(source) = tokio::fs::write(&path, assistant.as_bytes()).await
    {
        return Err(PrintError::OutputFile { path, source });
    }

    if opts.json {
        let reason = match stop {
            Stop::EndTurn => "end_turn",
            Stop::Length => "max_tokens",
            Stop::MaxSteps => "max_turn_requests",
            Stop::Cancelled => "cancelled",
            Stop::Filter => "refused",
            Stop::Failed => return Err(PrintError::FailedWithoutMessage),
        };
        let line = dal_wire::acp_prompt_result(reason);
        stdout
            .write_all(line.as_bytes())
            .await
            .map_err(PrintError::Io)?;
    } else {
        if assistant.is_empty() {
            stderr
                .write_all(dal_texts::EMPTY_MESSAGE.as_bytes())
                .await
                .map_err(PrintError::Io)?;
            stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
        }
        stdout.write_all(b"\n").await.map_err(PrintError::Io)?;
    }
    stdout.flush().await.map_err(PrintError::Io)?;
    if let Some(busy) = quiet_end {
        return Err(PrintError::NotQuiet {
            busy,
            wait: opts.quiet_wait.unwrap_or(QUIET_WAIT),
        });
    }
    Ok(PrintOutcome::Completed)
}

enum QuietEnd {
    Quiet,
    Stopped,
    TimedOut(Box<str>),
}

async fn await_quiet(
    agent: &Agent,
    stop: &CancellationToken,
    wait: Option<Duration>,
) -> Result<QuietEnd, PrintError> {
    let deadline = wait.map(|wait| tokio::time::Instant::now() + wait);
    loop {
        let busy: Vec<Box<str>> = agent
            .poll_status()
            .await?
            .into_iter()
            .filter(|status| !status.is_quiet())
            .map(|status| status.ext)
            .collect();
        if busy.is_empty() {
            return Ok(QuietEnd::Quiet);
        }
        let names: Box<str> = busy.join(", ").into();
        let expiry = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(QuietEnd::Stopped),
            () = expiry => return Ok(QuietEnd::TimedOut(names)),
            () = tokio::time::sleep(QUIET_RECHECK) => {}
        }
    }
}

async fn write_failure(
    error: &PrintError,
    json: bool,
    stdout: &mut (impl AsyncWrite + Unpin),
    stderr: &mut (impl AsyncWrite + Unpin),
) -> Result<(), PrintError> {
    let (what, hint) = error.diagnostic();
    if json {
        let line = dal_wire::acp_prompt_error(-32000, &what, &hint);
        stdout
            .write_all(line.as_bytes())
            .await
            .map_err(PrintError::Io)?;
        stdout.flush().await.map_err(PrintError::Io)?;
        return Ok(());
    }
    stderr
        .write_all(what.as_bytes())
        .await
        .map_err(PrintError::Io)?;
    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
    stderr
        .write_all(hint.as_bytes())
        .await
        .map_err(PrintError::Io)?;
    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
    Ok(())
}

use crate::cli::texts as dal_texts;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use dal_agent::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
    use dal_agent::{Agent, Env, Host, Product, SessionRef};
    use dal_core::{ClientId, Config, ConfigProduct, Part, ServiceSet, Workspace};
    use tokio_util::sync::CancellationToken;

    use super::{PrintError, PrintOptions, PrintOutcome, run_print};

    const FIXTURE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hello\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

    struct Switch(Mutex<bool>);

    impl Switch {
        fn set_quiet(&self, quiet: bool) {
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = quiet;
        }
    }

    impl StatusPoll for Switch {
        fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
            let quiet = *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            StatusSnapshot { quiet, text: None }
        }
    }

    async fn session(switch: &Arc<Switch>) -> (tempfile::TempDir, Host, Agent) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let workspace_dir = tmp.path().join("w");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, FIXTURE).expect("fixture");
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.display()
        );
        let config =
            Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
        let extension = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::default())
            .expect("builder")
            .status_kind("focus", Arc::clone(switch) as Arc<dyn StatusPoll>)
            .build()
            .expect("extension");
        let product = Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![extension],
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: workspace_dir.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await.expect("host");
        let agent = host
            .open(
                SessionRef::Ephemeral {
                    workspace: Workspace::new(workspace_dir).expect("workspace"),
                },
                ClientId::new("cli"),
            )
            .await
            .expect("open");
        (tmp, host, agent)
    }

    fn options(json: bool, quiet_wait: Option<Duration>, stop: &CancellationToken) -> PrintOptions {
        PrintOptions {
            json,
            output_last_message: None,
            prompt: vec![Part::Text { text: "hi".into() }],
            stderr_is_tty: false,
            stop: stop.clone(),
            quiet_wait,
            headless_approval: false,
        }
    }

    #[tokio::test]
    async fn json_waits_for_quiet_after_the_turn_then_writes_one_line() {
        let switch = Arc::new(Switch(Mutex::new(false)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        let release = Arc::clone(&switch);
        #[expect(
            clippy::disallowed_methods,
            reason = "test orchestration: the test runtime schedules its own quiet release"
        )]
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            release.set_quiet(true);
        });
        let started = Instant::now();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, options(true, None, &stop), &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert_eq!(
            String::from_utf8(stdout).expect("utf8"),
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{\"stopReason\":\"end_turn\"}}\n"
        );
        assert!(stderr.is_empty());
        host.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn json_never_quiet_waits_for_the_stop_signal() {
        let switch = Arc::new(Switch(Mutex::new(false)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        #[expect(
            clippy::disallowed_methods,
            reason = "test orchestration: the test runtime schedules its own cancel"
        )]
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            cancel.cancel();
        });
        let started = Instant::now();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, options(true, None, &stop), &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Interrupted);
        assert!(started.elapsed() >= Duration::from_millis(450));
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let report = host.shutdown(Duration::from_millis(100)).await;
        assert!(!report.status_quiet);
    }

    #[tokio::test]
    async fn text_run_exits_after_quiet_wait_with_status_not_quiet() {
        let switch = Arc::new(Switch(Mutex::new(false)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        let started = Instant::now();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let error = run_print(
            agent,
            options(false, Some(Duration::from_secs(1)), &stop),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect_err("still busy");
        assert!(matches!(error, PrintError::NotQuiet { .. }));
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(String::from_utf8(stdout).expect("utf8"), "Hello\n");
        assert_eq!(
            String::from_utf8(stderr).expect("utf8"),
            "dalgon: extension status is not quiet: focus still busy after 1 second\nLet the extension finish and run dalgon again, or use --json to wait until it is quiet.\n"
        );
        host.shutdown(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn quiet_session_finishes_without_waiting() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(
            agent,
            options(false, Some(Duration::from_secs(1)), &stop),
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("run");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert_eq!(String::from_utf8(stdout).expect("utf8"), "Hello\n");
        assert!(stderr.is_empty());
        host.shutdown(Duration::from_secs(1)).await;
    }
}
