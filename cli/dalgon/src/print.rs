//! Headless prompt streaming for text and JSON clients.

use std::borrow::Cow;
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
    reason = "process edge: print mode reads prompt text from standard input"
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
    pub stdout_is_tty: bool,
    pub stderr_is_tty: bool,
    /// Approval mode: `ask` with no terminal emits the once-per-run headless
    /// notice before the first denial note.
    pub approval: dal_core::ApprovalMode,
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
        matches!(self, Self::Io(error) if crate::exit::broken_pipe(error).is_some())
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
    let stderr_is_tty = opts.stderr_is_tty;
    match run_print_inner(agent, opts, stdout, stderr).await {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            if !error.is_broken_pipe() {
                write_failure(&error, json, stderr_is_tty, stdout, stderr).await?;
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
    // asks into a headless denial instead of a broker timeout.
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

    // The once-per-run headless notice (N0) is emitted lazily below, once
    // before the first per-call denial note, so runs with no denials stay
    // silent on stderr.

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
                    write_terminal_text(stdout, text, opts.stdout_is_tty).await?;
                }
            }
            UpdateKind::ToolStarted { tool, .. } if opts.stderr_is_tty && !opts.json => {
                let progress = format!("Running {tool}.\n");
                write_terminal_text(stderr, &progress, opts.stderr_is_tty).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } if !opts.json => {
                if !outcome.is_error {
                    continue;
                }
                let Some((tool, rung)) = parse_headless_denial(&outcome.text) else {
                    continue;
                };
                if denial_count == 0 && opts.approval == dal_core::ApprovalMode::Ask {
                    stderr
                        .write_all(dal_texts::HEADLESS_APPROVAL.as_bytes())
                        .await
                        .map_err(PrintError::Io)?;
                    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
                }
                let note = dal_texts::approval_denied_note(tool, rung.as_str());
                write_terminal_text(stderr, &note, opts.stderr_is_tty).await?;
                stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
                denial_count += 1;
            }
            UpdateKind::Notice(notice) => {
                last_notice = Some(notice.text.clone());
                if !opts.json {
                    write_terminal_text(stderr, &notice.text, opts.stderr_is_tty).await?;
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

    emit_print_result(
        stop,
        denial_count,
        &assistant,
        quiet_end,
        opts.output_last_message,
        opts.json,
        opts.quiet_wait,
        last_notice,
        stdout,
        stderr,
    )
    .await
}

/// Emits the turn's terminal output: failure mapping, denial short-circuit,
/// the optional last-message file, and the json or text trailer.
async fn emit_print_result(
    stop: Stop,
    denial_count: usize,
    assistant: &str,
    quiet_end: Option<Box<str>>,
    output_last_message: Option<PathBuf>,
    json: bool,
    quiet_wait: Option<Duration>,
    last_notice: Option<Box<str>>,
    stdout: &mut (impl AsyncWrite + Unpin),
    stderr: &mut (impl AsyncWrite + Unpin),
) -> Result<PrintOutcome, PrintError> {
    if stop == Stop::Failed {
        return Err(last_notice.map_or(PrintError::FailedWithoutMessage, PrintError::TurnFailed));
    }

    // One exact note per denied call plus the turn-end summary count. A
    // denied run is a completed run with nothing to show: exit non-zero.
    if denial_count > 0 && !json {
        let summary = denial_summary(denial_count);
        stderr
            .write_all(summary.as_bytes())
            .await
            .map_err(PrintError::Io)?;
        stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
        return Ok(PrintOutcome::Denied);
    }

    if let Some(path) = output_last_message
        && let Err(source) = tokio::fs::write(&path, assistant.as_bytes()).await
    {
        return Err(PrintError::OutputFile { path, source });
    }

    if json {
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
            wait: quiet_wait.unwrap_or(QUIET_WAIT),
        });
    }
    Ok(PrintOutcome::Completed)
}

/// The turn-end denial summary (`dalgon: <n> call(s) denied by approval
/// mode.`), with the plural `s` only when more than one call was denied.
fn denial_summary(denial_count: usize) -> String {
    let plural = if denial_count == 1 { "" } else { "s" };
    format!("dalgon: {denial_count} call{plural} denied by approval mode.")
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
    stderr_is_tty: bool,
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
    write_terminal_text(stderr, &what, stderr_is_tty).await?;
    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
    write_terminal_text(stderr, &hint, stderr_is_tty).await?;
    stderr.write_all(b"\n").await.map_err(PrintError::Io)?;
    Ok(())
}

/// Escapes untrusted text only for an interactive terminal.
async fn write_terminal_text(
    output: &mut (impl AsyncWrite + Unpin),
    text: &str,
    is_tty: bool,
) -> Result<(), PrintError> {
    let displayed = if is_tty {
        dal_core::terminal::escape_preserving_newlines(text)
    } else {
        Cow::Borrowed(text)
    };
    output
        .write_all(displayed.as_bytes())
        .await
        .map_err(PrintError::Io)
}

use crate::cli::texts as dal_texts;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use dal_agent::error::ToolError;
    use dal_agent::ext::tool::{
        ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput,
    };
    use dal_agent::ext::{BoxFuture, ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};
    use dal_agent::{Agent, Env, Host, Product, SessionRef};
    use dal_core::{
        ClientId, Config, ConfigProduct, ModelInfo, Name, Part, Preview, RawJson, ServiceSet,
        ToolClass, ToolSpec, Visibility, Workspace,
    };
    use tokio_util::sync::CancellationToken;

    use super::{PrintError, PrintOptions, PrintOutcome, run_print, write_failure};

    const CONTROL_FIXTURE: &str = concat!(
        r#"{"kind":"events","events":[{"type":"text_delta","text":"Hello\n\u001b]52;c;SGV"},{"type":"text_delta","text":"sbG8=\u0007\u009b2J\u007f\u202e\rafter\n"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}"#,
        "\n",
    );
    const CONTROL_TEXT: &str = "Hello\n\x1b]52;c;SGVsbG8=\x07\u{9b}2J\u{7f}\u{202e}\rafter\n";

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
        session_with_fixture(switch, FIXTURE).await
    }

    async fn session_with_fixture(
        switch: &Arc<Switch>,
        content: &str,
    ) -> (tempfile::TempDir, Host, Agent) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let workspace_dir = tmp.path().join("w");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, content).expect("fixture");
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
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

    const DENIAL_FIXTURE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"tool_call_started\",\"id\":\"c1\",\"name\":\"editgate\"},{\"type\":\"tool_call_started\",\"id\":\"c2\",\"name\":\"execgate\"},{\"type\":\"tool_calls_done\",\"calls\":[{\"id\":\"c1\",\"name\":\"editgate\",\"args\":{\"kind\":\"parsed\",\"value\":{}}},{\"id\":\"c2\",\"name\":\"execgate\",\"args\":{\"kind\":\"parsed\",\"value\":{}}}]}},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":4,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}\n{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Hello\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":12,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

    /// An approval-gated test tool: Patch class for `editgate`, Exec class
    /// for `execgate`. With nobody attached to answer, `ask` denies both as
    /// headless denials naming the `edits` and `all` rungs.
    struct GatedTool {
        name: Name,
        spec: Arc<ToolSpec>,
        patch: bool,
    }

    impl GatedTool {
        fn gated(name: &str, patch: bool) -> Self {
            let name = Name::parse(name).expect("tool name");
            let spec = Arc::new(ToolSpec {
                name: name.clone(),
                description: "Approval-gated marker tool.".into(),
                parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                    .expect("schema json"),
                grammar: None,
            });
            Self { name, spec, patch }
        }

        fn patch() -> Self {
            Self::gated("editgate", true)
        }

        fn exec() -> Self {
            Self::gated("execgate", false)
        }
    }

    impl Tool for GatedTool {
        fn name(&self) -> &Name {
            &self.name
        }

        fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
            Arc::clone(&self.spec)
        }

        fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
            if self.patch {
                Ok(ToolClass::Patch)
            } else {
                Ok(ToolClass::Exec {
                    read_only: false,
                    grant: None,
                })
            }
        }

        fn run<'a>(&'a self, _call: ToolCall, mut cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                let preview = Preview {
                    title: "gated".into(),
                    body: String::new().into(),
                    digest: None,
                };
                match cx.authorize(preview).await {
                    Ok(_) => ToolOutcome::Ok(Box::new(ToolOutput::from_text("allowed"))),
                    Err(reason) => ToolOutcome::Err(ToolError::Denied(reason)),
                }
            })
        }
    }

    async fn session_with_denial_tools(switch: &Arc<Switch>) -> (tempfile::TempDir, Host, Agent) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        let workspace_dir = tmp.path().join("w");
        std::fs::create_dir_all(&data).expect("data dir");
        std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, DENIAL_FIXTURE).expect("fixture");
        let user = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        );
        let config =
            Config::load(ConfigProduct::Dalgon, &data, "", Some(user.as_str())).expect("config");
        let extension = ExtensionBuilder::new("focus", "0.1.0", ServiceSet::default())
            .expect("builder")
            .status_kind("focus", Arc::clone(switch) as Arc<dyn StatusPoll>)
            .tool(Arc::new(GatedTool::patch()), Visibility::Model)
            .tool(Arc::new(GatedTool::exec()), Visibility::Model)
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
            stdout_is_tty: false,
            stderr_is_tty: false,
            stop: stop.clone(),
            quiet_wait,
            approval: dal_core::ApprovalMode::All,
        }
    }

    #[tokio::test]
    async fn tty_print_escapes_split_terminal_sequences_and_preserves_last_message() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (tmp, host, agent) = session_with_fixture(&switch, CONTROL_FIXTURE).await;
        let stop = CancellationToken::new();
        let output_path = tmp.path().join("last-message.txt");
        let mut opts = options(false, None, &stop);
        opts.stdout_is_tty = true;
        opts.output_last_message = Some(output_path.clone());
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, opts, &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert_eq!(
            String::from_utf8(stdout).expect("utf8"),
            "Hello\n\\u{1b}]52;c;SGVsbG8=\\a\\u{9b}2J\\u{7f}\\u{202e}\\rafter\n\n"
        );
        assert_eq!(
            std::fs::read(output_path).expect("saved message"),
            CONTROL_TEXT.as_bytes()
        );
        assert_eq!(stderr, [] as [u8; 0]);
        host.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn redirected_print_keeps_provider_bytes_unchanged() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (_tmp, host, agent) = session_with_fixture(&switch, CONTROL_FIXTURE).await;
        let stop = CancellationToken::new();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, options(false, None, &stop), &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert_eq!(stdout, [CONTROL_TEXT.as_bytes(), b"\n"].concat());
        assert_eq!(stderr, [] as [u8; 0]);
        host.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn terminal_diagnostics_escape_untrusted_controls() {
        let error = PrintError::TurnFailed("\x1b]2;spoof\x07".into());
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        write_failure(&error, false, true, &mut stdout, &mut stderr)
            .await
            .expect("write");
        assert_eq!(stdout, [] as [u8; 0]);
        assert_eq!(
            String::from_utf8(stderr).expect("utf8"),
            "\\u{1b}]2;spoof\\a\nCheck the provider response and try the prompt again.\n"
        );
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
        assert_eq!(stderr, [] as [u8; 0]);
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
        assert_eq!(stdout, [] as [u8; 0]);
        assert_eq!(stderr, [] as [u8; 0]);
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
    async fn quiet_session_returns_well_under_its_wait() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        // Deterministic deadline proof on the real quiet-wait path: freeze
        // the virtual clock around `await_quiet` with the original 1s wait.
        // An already-quiet agent returns without arming any timer, so virtual
        // time does not advance; an unconditional sleep auto-advances it.
        tokio::time::pause();
        let start = tokio::time::Instant::now();
        let end = super::await_quiet(&agent, &stop, Some(Duration::from_secs(1)))
            .await
            .expect("quiet");
        let quiet_elapsed = start.elapsed();
        assert!(matches!(end, super::QuietEnd::Quiet));
        assert_eq!(
            quiet_elapsed,
            Duration::ZERO,
            "already-quiet session must not advance its 1s wait: {quiet_elapsed:?}"
        );
        tokio::time::resume();
        // Functional half preserves the Completed/stdout/stderr contract with
        // the original 1s option; the whole-turn real I/O is not timed.
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
        assert_eq!(stderr, [] as [u8; 0]);
        host.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn ask_run_without_denials_prints_no_headless_notice() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (_tmp, host, agent) = session(&switch).await;
        let stop = CancellationToken::new();
        let mut opts = options(false, None, &stop);
        opts.approval = dal_core::ApprovalMode::Ask;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, opts, &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert_eq!(String::from_utf8(stdout).expect("utf8"), "Hello\n");
        assert_eq!(stderr, [] as [u8; 0]);
        host.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn denied_text_run_ends_with_a_counted_summary() {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = super::emit_print_result(
            dal_core::Stop::EndTurn,
            2,
            "Hello",
            None,
            None,
            false,
            None,
            None,
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("summary");
        assert_eq!(outcome, PrintOutcome::Denied);
        assert_eq!(stdout, [] as [u8; 0]);
        assert_eq!(
            String::from_utf8(stderr).expect("utf8"),
            "dalgon: 2 calls denied by approval mode.\n"
        );
    }

    #[tokio::test]
    async fn single_denial_summary_uses_the_singular_call() {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = super::emit_print_result(
            dal_core::Stop::EndTurn,
            1,
            "Hello",
            None,
            None,
            false,
            None,
            None,
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("summary");
        assert_eq!(outcome, PrintOutcome::Denied);
        assert_eq!(stdout, [] as [u8; 0]);
        assert_eq!(
            String::from_utf8(stderr).expect("utf8"),
            "dalgon: 1 call denied by approval mode.\n"
        );
    }

    #[tokio::test]
    async fn denied_json_run_emits_no_denial_text() {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = super::emit_print_result(
            dal_core::Stop::EndTurn,
            2,
            "Hello",
            None,
            None,
            true,
            None,
            None,
            &mut stdout,
            &mut stderr,
        )
        .await
        .expect("summary");
        assert_eq!(outcome, PrintOutcome::Completed);
        assert_eq!(
            String::from_utf8(stdout).expect("utf8"),
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{\"stopReason\":\"end_turn\"}}\n"
        );
        assert_eq!(stderr, [] as [u8; 0]);
    }

    #[tokio::test]
    async fn denied_text_run_reports_each_denial_once_with_summary() {
        let switch = Arc::new(Switch(Mutex::new(true)));
        let (_tmp, host, agent) = session_with_denial_tools(&switch).await;
        let stop = CancellationToken::new();
        let mut opts = options(false, None, &stop);
        opts.approval = dal_core::ApprovalMode::Ask;
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let outcome = run_print(agent, opts, &mut stdout, &mut stderr)
            .await
            .expect("run");
        assert_eq!(outcome, PrintOutcome::Denied);
        assert_eq!(String::from_utf8(stdout).expect("utf8"), "Hello");
        assert_eq!(
            String::from_utf8(stderr).expect("utf8"),
            concat!(
                "dalgon: approval is ask and no terminal is attached: patch and exec calls are denied. Re-run with --approval all to approve them.\n",
                "dalgon: editgate needs approval; print mode cannot ask. Rerun with --approval edits.\n",
                "dalgon: execgate needs approval; print mode cannot ask. Rerun with --approval all.\n",
                "dalgon: 2 calls denied by approval mode.\n"
            )
        );
        host.shutdown(Duration::from_secs(1)).await;
    }
}
