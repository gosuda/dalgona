//! The `dalgon` command-line and process edge for the dal product.
#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use dal_agent::{Env, HostError, SessionRef};
use dal_core::{
    ApprovalMode, ClientId, ConfigOverrides, ConfigProduct, Mode, Screen, ThinkingLevel, Workspace,
};
use tokio::io::AsyncWriteExt as _;

/// The assembled product a binary or harness serves through a [`Host`].
pub use dal_agent::Product;
/// Product configuration shared with the edge builder.
pub use dal_core::Config;
/// Typed product-configuration failures.
pub use dal_core::ConfigError;
/// Typed extension-registration failures.
pub use dal_core::RegistrationError;
/// One first-party documentation scheme and its pages.
pub use dal_ext::docs::Manual as ProductManual;
/// The scheme resolver that serves an extension's doc pages to the `read` tool.
pub use dal_ext::docs::ManualScheme;

/// Captured inputs supplied to a product constructor.
#[derive(Debug)]
pub struct BuildCx<'a> {
    /// Absolute, product-specific data root.
    pub data_root: PathBuf,
    /// Strictly loaded and validated product configuration.
    pub config: &'a Config,
}

/// Failure to finish constructing a product.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// A built-in extension failed registration.
    #[error(transparent)]
    Registration(#[from] RegistrationError),
    /// Core-owned configuration could not be decoded or validated.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// An extension-owned configuration section failed validation.
    #[error("{source}")]
    Section {
        /// The TOML section owned by the extension.
        section: Box<str>,
        /// The original typed section error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The single product constructor shared by every `dalgon` binary alias.
///
/// Each alias passes its own factory into the edge entry point; the build
/// callback receives the captured [`BuildCx`] exactly once at the edge.
#[derive(Clone, Copy)]
pub struct ProductFactory {
    /// Binary identity selecting the version line (`dalgon`, `dal`, or `dl`).
    pub binary: &'static str,
    /// Product version rendered on the `--version` line.
    ///
    /// Each product bakes its own crate version here: the dalgon and dalgona
    /// products version independently, so the shared edge must not read the
    /// version from the crate it was compiled in.
    pub version: &'static str,
    /// Embedded product-default TOML layered after dal code defaults.
    pub defaults: &'static str,
    /// Configuration kind deciding the built-in layer and accepted keys.
    pub config_product: ConfigProduct,
    /// Builds the architecture product from the captured edge context.
    pub build: fn(&BuildCx<'_>) -> Result<dal_agent::Product, BuildError>,
    /// Returns every first-party manual, without loading user plugins.
    pub docs: fn() -> Vec<ProductManual>,
    /// Strictly decodes the product-owned sections of a loaded [`Config`] and
    /// returns its documented, non-secret, startup-only editable settings.
    ///
    /// The interactive settings picker lists these rows and validates every
    /// candidate `dal.toml` edit through this same decoder before writing.
    pub settings: fn(&Config) -> Result<Vec<dal_core::ProductSetting>, BuildError>,
}

/// The single Clap command tree shared by parsing, help, completion, and man pages.
pub mod cli;

/// Command dispatch over the built product.
mod dispatch;
#[cfg(all(feature = "test-support", any(unix, windows)))]
pub use dispatch::{RawWireStreams, raw_wire_streams};

/// Process-edge snapshots, lexical paths, and platform identity.
mod edge;
pub use edge::{EdgeError, RootPaths, config_file_path, resolve_roots};

/// Exit-code mapping for command and signal outcomes.
mod exit;
/// The bounded process log sink.
mod log;

/// Commands that inspect and persist configured plugin grants.
pub mod plugin_cmd;

/// Headless prompt streaming for text and JSON clients.
mod print;

/// The dal product identity and built-in extension composition.
pub mod product;

/// Offline commands for inspecting and testing stream rules.
pub mod rules_cmd;

/// The restricted `__sandbox` helper entry point.
mod sandbox;

/// Local HTTP, WebSocket, and Codex serving commands.
pub mod serve;
pub use product::{Parts, assemble, build, parts, product};

use std::path::{Path, PathBuf};

/// The environment captured once at the process edge.
pub(crate) type VarsMap = std::collections::BTreeMap<std::ffi::OsString, OsString>;

/// The resolved inputs carried from startup into command dispatch.
pub(crate) struct Startup {
    pub(crate) vars: VarsMap,
    pub(crate) cwd: PathBuf,
    pub(crate) workspace_path: PathBuf,
    pub(crate) workspace: Workspace,
    pub(crate) config: Config,
    /// The user `dal.toml` path; default-setting persistence writes to it.
    pub(crate) config_path: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) helper: Option<PathBuf>,
    /// Whether the user `dal.toml` sets `[serve] bind`. Only a user-set bind
    /// outranks `--public`'s wildcard default; the compiled default does not.
    pub(crate) serve_bind_explicit: bool,
}

/// Runs the process edge for one product factory.
///
/// Captures the environment once, resolves family roots from
/// [`ProductFactory::binary`], layers configuration (dal code defaults,
/// factory defaults, the user `dal.toml`, then CLI flags), builds the product
/// once, and dispatches to the selected command. Every failure exits through
/// the typed [`exit`] map; only the binary target observes the status.
#[must_use]
pub fn run(factory: ProductFactory) -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    if argv.get(1).is_some_and(|arg| arg == "__sandbox") {
        return sandbox::run(&argv);
    }
    let cli = match cli::parse_cli(factory.binary, factory.version, argv) {
        Ok(cli) => cli,
        Err(error) => return clap_exit(&error),
    };
    if let Err(validation) = cli::validate_root(&cli) {
        return root_validation_exit(validation);
    }
    let (vars, cwd) = match capture_process() {
        Ok(captured) => captured,
        Err(code) => return code,
    };
    #[cfg(all(feature = "test-support", unix))]
    let vars = match claim_controlling_pty(vars) {
        Ok(vars) => vars,
        Err(code) => return code,
    };
    // The tracing dispatcher is process lifetime: from here, product
    // events on every thread and task forward into the installed sink.
    log::install_tracing();
    // `dalgon dev` drives its own data root and workspace: broken user
    // configuration or product assembly must not block journal surgery
    // or a scenario run, so it dispatches before either is built.
    if let Some(cli::Commands::Dev(args)) = &cli.command {
        let runtime = match tokio_runtime(&vars, factory.binary) {
            Ok(runtime) => runtime,
            Err(code) => return code,
        };
        let code = runtime.block_on(dispatch::dev(args, factory, vars, cwd, edge::current_exe()));
        runtime.shutdown_timeout(Duration::from_millis(500));
        return code;
    }
    let startup = match assemble_startup(&factory, &cli, vars, cwd) {
        Ok(startup) => startup,
        Err(code) => return code,
    };
    let sink = open_log_sink(
        edge::parse_log_level(&startup.vars).ok().flatten(),
        &startup.data_root,
    );
    if let Some(cli::Commands::Docs(args)) = &cli.command {
        let code = dispatch::docs(args.clone(), factory.binary, (factory.docs)());
        flush_log(sink.as_ref(), exit::flush_budget());
        return code;
    }
    let product = match build_once(&factory, startup.data_root.clone(), &startup.config) {
        Ok(product) => product,
        Err(code) => {
            flush_log(sink.as_ref(), exit::flush_budget());
            return code;
        }
    };
    let runtime = match tokio_runtime(&startup.vars, factory.binary) {
        Ok(runtime) => runtime,
        Err(code) => {
            flush_log(sink.as_ref(), exit::flush_budget());
            return code;
        }
    };
    let code = runtime.block_on(run_command(factory, cli, startup, product));
    runtime.shutdown_timeout(exit::runtime_budget());
    flush_log(sink.as_ref(), exit::flush_budget());
    code
}

/// Builds the shared multi-thread runtime, mapping a build failure to the
/// edge's typed exit.
fn tokio_runtime(
    vars: &VarsMap,
    binary: &'static str,
) -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .build()
        .map_err(|error| {
            let log_path = edge::log_file_path(vars, binary);
            two_lines(
                cli::texts::internal_error_at(
                    "edge",
                    &format!("cannot start the runtime: {error}"),
                    &log_path,
                ),
                exit::ExitKind::Internal,
            )
        })
}

/// Dispatches the parsed command over the built product.
async fn run_command(
    factory: ProductFactory,
    cli: cli::Cli,
    startup: Startup,
    product: dal_agent::Product,
) -> ExitCode {
    match &cli.command {
        None if cli.print || cli.json || !cli.prompts.is_empty() => {
            let Startup {
                vars,
                cwd,
                workspace_path,
                workspace,
                config,
                config_path,
                data_root,
                helper,
                ..
            } = startup;
            run_headless(
                &cli,
                Env {
                    vars,
                    cwd,
                    sandbox_helper: helper,
                },
                workspace_path,
                workspace,
                config,
                &config_path,
                product,
                &data_root,
            )
            .await
        }
        #[cfg(feature = "tui")]
        None => Box::pin(dispatch::interactive(&cli, startup, factory, product)).await,
        #[cfg(not(feature = "tui"))]
        None => two_lines(
            [
                cli::texts::NO_PROMPT.into(),
                cli::texts::NO_PROMPT_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
        Some(cli::Commands::Completion(args)) => {
            let script = cli::completion_script(args.shell, factory.binary, factory.version);
            let _ = write!(std::io::stdout().lock(), "{script}");
            exit::code(exit::ExitKind::Success)
        }
        Some(cli::Commands::Sandbox) => {
            unreachable!("the __sandbox helper routes before any path work")
        }
        Some(cli::Commands::Serve(args)) => {
            dispatch::serve(&cli, args.clone(), startup, product).await
        }
        Some(cli::Commands::Plugin(args)) => dispatch::plugin(args.clone(), startup, product).await,
        Some(cli::Commands::Rules(args)) => dispatch::rules(args.clone(), startup, product).await,
        Some(cli::Commands::Dev(_)) => {
            unreachable!("the dev command dispatches before product assembly")
        }
        Some(cli::Commands::Login(args)) => dispatch::login(args.clone(), startup, product).await,
        Some(cli::Commands::Logout(args)) => dispatch::logout(args.clone(), startup, product).await,
        Some(cli::Commands::Models(args)) => dispatch::models(args.clone(), startup).await,
        Some(cli::Commands::Docs(_)) => two_lines(
            cli::texts::internal_error("edge", "the docs command was dispatched twice"),
            exit::ExitKind::Internal,
        ),
        Some(cli::Commands::Rpc(args)) => dispatch::rpc(args.clone(), startup, product).await,
        Some(cli::Commands::Acp) => dispatch::acp(startup, product).await,
        Some(cli::Commands::AppServer) => dispatch::app_server(startup, product).await,
    }
}

/// Runs one headless prompt turn over the host agent.
async fn run_headless(
    cli: &cli::Cli,
    env: Env,
    workspace_path: PathBuf,
    workspace: Workspace,
    config: Config,
    config_path: &Path,
    product: dal_agent::Product,
    data_root: &Path,
) -> ExitCode {
    if config.model().is_none() {
        return two_lines(
            [
                cli::texts::NO_MODEL.into(),
                cli::texts::NO_MODEL_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    let snapshot = edge::terminal_snapshot();
    let parts =
        match print::assemble_prompt(&cli.prompts, &workspace_path, snapshot.stdin_tty).await {
            Ok(parts) => parts,
            Err(error) => return prompt_error_exit(error),
        };
    // Print mode runs without a terminal: under `ask` the run cannot answer
    // approval requests, so it emits the once-per-run headless notice and
    // denies gated calls instead of waiting out the broker timeout.
    let approval = config.approval();
    let session = session_ref(cli, workspace);
    let host = match dispatch::start_host(product, config, env, data_root, config_path).await {
        Ok(host) => host,
        Err(code) => return code,
    };
    let agent = match host.open(session, ClientId::new("cli")).await {
        Ok(agent) => agent,
        Err(error) => {
            let code = host_exit(&error, data_root);
            let _ = host.shutdown(Duration::from_secs(3)).await;
            return code;
        }
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let options = print::PrintOptions {
        json: cli.json,
        output_last_message: cli.output_last_message.clone(),
        prompt: parts,
        stdout_is_tty: snapshot.stdout_tty,
        stderr_is_tty: snapshot.stderr_tty,
        approval,
        stop: stop.clone(),
        quiet_wait: (!cli.json).then_some(print::QUIET_WAIT),
    };
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut signals = edge::ShutdownSignals::register();
    let outcome = match dispatch::drive(
        &stop,
        &mut signals,
        print::run_print(agent, options, &mut stdout, &mut stderr),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(code) => {
            let _closed = exit::Ladder::begin()
                .close_host_after_quiet_wait(host)
                .await;
            return ExitCode::from(code);
        }
    };
    // The print run already waited for status kinds to go quiet when it
    // timed out on them, so the host must not spend that grace again.
    let quiet_waited = matches!(&outcome, Err(print::PrintError::NotQuiet { .. }));
    let ladder = exit::Ladder::begin();
    let closed = if quiet_waited {
        ladder.close_host_after_quiet_wait(host).await
    } else {
        ladder.close_host(host, &mut signals).await
    };
    if let Some(code) = closed.skipped_by {
        return ExitCode::from(code);
    }
    if !closed.report.status_quiet && !quiet_waited {
        let [what, hint] = cli::texts::status_not_quiet_shutdown();
        let _ = stderr
            .write_all(format!("{what}\n{hint}\n").as_bytes())
            .await;
    }
    match outcome {
        Ok(print::PrintOutcome::Completed) => exit::code(exit::ExitKind::Success),
        Ok(print::PrintOutcome::Interrupted) => exit::code(exit::ExitKind::Signal(2)),
        Err(error) if error.is_broken_pipe() => exit::code(exit::ExitKind::Signal(13)),
        // A headless denial is a requested failure like a tool error.
        Ok(print::PrintOutcome::Denied) | Err(_) => exit::code(exit::ExitKind::RequestedFailure),
    }
}

/// Maps a prompt assembly failure to its usage diagnostics.
fn prompt_error_exit(error: print::PromptError) -> ExitCode {
    match error {
        print::PromptError::Empty => two_lines(
            [
                cli::texts::NO_PROMPT.into(),
                cli::texts::NO_PROMPT_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
        print::PromptError::DashWithoutPipe => two_lines(
            [
                cli::texts::DASH_NEEDS_STDIN.into(),
                cli::texts::DASH_NEEDS_STDIN_HINT.into(),
            ],
            exit::ExitKind::Usage,
        ),
        print::PromptError::FileMissing { path, arg } => two_lines(
            cli::texts::prompt_file_not_found(&path, &arg),
            exit::ExitKind::RequestedFailure,
        ),
        print::PromptError::FileUnreadable { path, source } => two_lines(
            cli::texts::prompt_file_unreadable(&path, &source.to_string()),
            exit::ExitKind::RequestedFailure,
        ),
        print::PromptError::FileNotUtf8 { path } => two_lines(
            cli::texts::prompt_file_not_utf8(&path),
            exit::ExitKind::RequestedFailure,
        ),
        print::PromptError::Stdin(source) => two_lines(
            cli::texts::stdin_unreadable(&source.to_string()),
            exit::ExitKind::RequestedFailure,
        ),
    }
}

/// Maps a rejected root-argument combination to its usage diagnostics.
fn root_validation_exit(validation: cli::RootValidation) -> ExitCode {
    let lines: [String; 2] = match validation {
        cli::RootValidation::ContinueWithResume => [
            cli::texts::CONTINUE_RESUME_CONFLICT.into(),
            cli::texts::CONTINUE_RESUME_HINT.into(),
        ],
        cli::RootValidation::ModeWithModeId => [
            cli::texts::MODE_CONFLICT.into(),
            cli::texts::MODE_CONFLICT_HINT.into(),
        ],
        cli::RootValidation::EmptyName => [
            cli::texts::EMPTY_NAME.into(),
            cli::texts::EMPTY_NAME_HINT.into(),
        ],
        cli::RootValidation::ConnectHeadless => [
            cli::texts::CONNECT_HEADLESS.into(),
            cli::texts::CONNECT_HEADLESS_HINT.into(),
        ],
    };
    two_lines(lines, exit::ExitKind::Usage)
}

/// Builds the session reference named by the root flags.
fn session_ref(cli: &cli::Cli, workspace: Workspace) -> SessionRef {
    if cli.no_session {
        return SessionRef::Ephemeral { workspace };
    }
    if cli.continue_session {
        return SessionRef::Continue { workspace };
    }
    match (&cli.resume, &cli.name) {
        (Some(None), _) => SessionRef::Continue { workspace },
        (Some(Some(key)), _) => SessionRef::Resume {
            key: key.as_str().into(),
            workspace,
        },
        (None, name) => SessionRef::New {
            workspace,
            name: name.as_deref().map(Into::into),
        },
    }
}

/// Splits `--mode` and a mode-form `--model dalgon/<mode>` into overrides.
fn split_mode_model(
    mode: Option<cli::ModeArg>,
    model: Option<String>,
) -> (Option<Mode>, Option<String>) {
    if let Some(mode) = mode {
        return (Some(map_mode(mode)), model);
    }
    match model {
        Some(id) => match dal_wire::router::HarnessMode::parse(&id) {
            Some(dal_wire::router::HarnessMode::Normal) => (Some(Mode::Normal), None),
            Some(dal_wire::router::HarnessMode::EvalFirst) => (Some(Mode::EvalFirst), None),
            Some(dal_wire::router::HarnessMode::EvalOnly) => (Some(Mode::EvalOnly), None),
            None => (None, Some(id)),
        },
        None => (None, None),
    }
}

fn map_mode(arg: cli::ModeArg) -> Mode {
    match arg {
        cli::ModeArg::Normal => Mode::Normal,
        cli::ModeArg::EvalFirst => Mode::EvalFirst,
        cli::ModeArg::EvalOnly => Mode::EvalOnly,
    }
}

fn map_thinking(arg: cli::ThinkingArg) -> ThinkingLevel {
    match arg {
        cli::ThinkingArg::Off => ThinkingLevel::Off,
        cli::ThinkingArg::Minimal => ThinkingLevel::Minimal,
        cli::ThinkingArg::Low => ThinkingLevel::Low,
        cli::ThinkingArg::Medium => ThinkingLevel::Medium,
        cli::ThinkingArg::High => ThinkingLevel::High,
        cli::ThinkingArg::Xhigh => ThinkingLevel::Xhigh,
        cli::ThinkingArg::Max => ThinkingLevel::Max,
    }
}

pub(crate) fn map_approval(arg: cli::ApprovalArg) -> ApprovalMode {
    match arg {
        cli::ApprovalArg::Ask => ApprovalMode::Ask,
        cli::ApprovalArg::Edits => ApprovalMode::Edits,
        cli::ApprovalArg::All => ApprovalMode::All,
    }
}

fn map_screen(arg: cli::ScreenArg) -> Screen {
    match arg {
        cli::ScreenArg::Inline => Screen::Inline,
        cli::ScreenArg::Fullscreen => Screen::Fullscreen,
    }
}

/// Renders one Clap parse outcome without terminating the process.
fn clap_exit(error: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = write!(std::io::stdout().lock(), "{error}");
            exit::code(exit::ExitKind::Success)
        }
        _ => {
            let _ = write!(std::io::stderr().lock(), "{error}");
            exit::code(exit::ExitKind::Usage)
        }
    }
}

/// Renders one configuration failure with its fix line.
///
/// Corrupt compiled defaults are an internal invariant failure, not a user
/// configuration error: they exit with the internal status and point at the
/// process log.
fn config_exit(binary: &str, error: &ConfigError, log_path: &Path) -> ExitCode {
    if let ConfigError::DefaultsInvalid { message } = error {
        return two_lines(
            cli::texts::internal_error_at("config", message, log_path),
            exit::ExitKind::Internal,
        );
    }
    two_lines(
        config_error_lines(error, binary),
        exit::ExitKind::RequestedFailure,
    )
}

/// Renders one configuration failure to its two diagnostic lines without
/// exiting: the shared body of [`config_exit`].
///
/// The unknown-key catalog names the closest key at suggestion distance or
/// lists the product's key table in table order; both end with the config
/// docs pointer. Invalid `[rules]` and `[eval] uses` values, a non-table
/// `[judge]`, and a TOML syntax failure render through the lines their owner
/// supplies, so the CLI never rewrites a value or recomputes a position. Any
/// other error keeps its rendered text and the fix hint.
fn config_error_lines(error: &ConfigError, binary: &str) -> [String; 2] {
    if let Some(lines) = error
        .rules_lines(binary)
        .or_else(|| error.eval_lines(binary))
    {
        return lines;
    }
    if let Some([what, fix]) = error.judge_lines() {
        return [format!("{binary}: {}: {what}", edge::CONFIG_FILE_NAME), fix];
    }
    if let ConfigError::Syntax { line, message } = error {
        let place = line.map_or_else(
            || edge::CONFIG_FILE_NAME.to_owned(),
            |line| format!("{}:{line}", edge::CONFIG_FILE_NAME),
        );
        return [
            format!("{binary}: {place}: {message}"),
            cli::texts::CONFIG_FIX_HINT.into(),
        ];
    }
    if let ConfigError::UnknownKey {
        key,
        suggestion,
        known_keys,
    } = error
    {
        let first = format!(
            "{binary}: {}: unknown key \"{key}\"",
            edge::CONFIG_FILE_NAME
        );
        let second = match suggestion.as_deref() {
            Some(nearest) => {
                format!("Did you mean \"{nearest}\"? See dalgon docs dal://config.")
            }
            None => format!(
                "Known keys: {} See dalgon docs dal://config.",
                known_keys.join(", ")
            ),
        };
        return [first, second];
    }
    [
        format!("{binary}: {error}"),
        cli::texts::CONFIG_FIX_HINT.into(),
    ]
}

/// Renders one host startup or open failure with its fix line.
///
/// A busy session with a live serve advertisement names the endpoint to
/// attach to; any other busy session keeps the generic remote-attach hint.
pub(crate) fn host_exit(error: &HostError, data_root: &Path) -> ExitCode {
    if let HostError::SessionBusy { id, pid } = error
        && let Some(advisory) = pid.and_then(|pid| serve::active_advertisement(data_root, pid))
    {
        return two_lines(
            [
                format!(
                    "dalgon: session {id} is open in dalgon serve (process {})",
                    advisory.pid
                ),
                format!(
                    "Run dal --connect {} to attach to that host.",
                    advisory.websocket
                ),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    if let HostError::Provider(error) = error
        && let Some(fix) = error.fix()
    {
        let _ = writeln!(std::io::stderr().lock(), "dalgon: {error}");
        let _ = writeln!(std::io::stderr().lock(), "{fix}");
        return exit::code(exit::ExitKind::RequestedFailure);
    }
    let _ = writeln!(std::io::stderr().lock(), "dalgon: {error}");
    let hint = if matches!(error, HostError::SessionBusy { .. }) {
        cli::texts::CONNECT_HINT
    } else {
        cli::texts::HOST_FIX_HINT
    };
    let _ = writeln!(std::io::stderr().lock(), "{hint}");
    exit::code(exit::ExitKind::RequestedFailure)
}

/// Writes two diagnostic lines and returns the mapped status.
pub(crate) fn two_lines(lines: [String; 2], kind: exit::ExitKind) -> ExitCode {
    let [first, second] = lines;
    let _ = write!(std::io::stderr().lock(), "{first}\n{second}\n");
    exit::code(kind)
}

/// Captures the process environment, working directory, and log level once.
fn capture_process() -> Result<(VarsMap, PathBuf), ExitCode> {
    let vars = edge::snapshot_environment();
    let cwd = edge::process_cwd().map_err(|source| {
        two_lines(
            [
                format!("dalgon: cannot read the working directory: {source}"),
                "Run dalgon from a readable directory.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        )
    })?;
    if edge::parse_log_level(&vars).is_err() {
        let value = vars
            .get(std::ffi::OsStr::new("DAL_LOG"))
            .map_or_else(String::default, |value| {
                value.to_string_lossy().into_owned()
            });
        return Err(two_lines(
            [
                format!("dalgon: DAL_LOG \"{value}\" is invalid"),
                "Use one of error, warning, info, or debug.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ));
    }
    Ok((vars, cwd))
}

/// The test-only request flag for a foreground controlling terminal.
///
/// Only gate-launched children opt in: the PTY helper sets it before spawn,
/// and the already-executing child consumes it here. Release builds never
/// read it.
#[cfg(all(feature = "test-support", unix))]
const CONTROLLING_PTY_ENV: &str = "DAL_TEST_CONTROLLING_PTY";

/// Acquires the stdin PTY as this process's controlling terminal when the
/// test-only flag requests it, then consumes the flag.
///
/// Runs in the already-executing child: a new session via `setsid`, attaches
/// the stdin slave with `TIOCSCTTY`, and makes this process the foreground
/// group with `tcsetpgrp`. Every step uses the safe Rustix wrappers, with no
/// `pre_exec`, no parent-session mutation, and no new command or flag. The
/// flag is removed from the captured map so it does not enter the host's
/// provider, tool, or child snapshots. Setup failures exit
/// explicitly instead of falling back to plain PTY I/O.
#[cfg(all(feature = "test-support", unix))]
fn claim_controlling_pty(mut vars: VarsMap) -> Result<VarsMap, ExitCode> {
    let opted_in = vars
        .remove(std::ffi::OsStr::new(CONTROLLING_PTY_ENV))
        .is_some_and(|value| !value.is_empty());
    if !opted_in {
        return Ok(vars);
    }
    if let Err(error) = attach_stdin_as_controlling_terminal() {
        return Err(two_lines(
            [
                format!("dalgon: cannot acquire the test controlling terminal: {error}"),
                "Run this gate through its PTY test harness.".into(),
            ],
            exit::ExitKind::Internal,
        ));
    }
    Ok(vars)
}

/// Attaches the stdin slave as this process's controlling terminal and
/// foreground group: a new session, `TIOCSCTTY` on stdin, then `tcsetpgrp`
/// to this process. Safe Rustix wrappers only; the caller maps the failure.
#[cfg(all(feature = "test-support", unix))]
fn attach_stdin_as_controlling_terminal() -> Result<(), rustix::io::Errno> {
    let stdin = rustix::stdio::stdin();
    rustix::process::setsid()?;
    rustix::process::ioctl_tiocsctty(stdin)?;
    let pid = rustix::process::getpid();
    rustix::termios::tcsetpgrp(stdin, pid)?;
    Ok(())
}

/// Opens the family log sink once after the roots resolve and installs it.
///
/// An unavailable log must not stop the product: the failure is reported on
/// stderr and the run continues without the sink rather than failing the
/// command.
fn open_log_sink(level: Option<edge::LogLevel>, data_root: &Path) -> Option<log::LogSink> {
    let log_path = edge::data_root_log_path(data_root);
    match log::LogSink::open(level, &log_path) {
        Ok(sink) => {
            // A second run in one process keeps the first sink in the slot;
            // this run still flushes its own handle.
            let _ = sink.install();
            Some(sink)
        }
        Err(error) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "dalgon: the log at {} is unavailable: {error}",
                log_path.display()
            );
            None
        }
    }
}

/// Flushes the process log sink at shutdown, after the runtime has stopped,
/// waiting at most `budget`.
///
/// The drop summary (`dropped <n> log lines`) is appended by the writer when
/// the count is non-zero; a flush failure is reported on stderr but never
/// changes the exit code.
pub(crate) fn flush_log(sink: Option<&log::LogSink>, budget: Duration) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = sink.flush(budget) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "dalgon: the log did not finish flushing: {error}"
        );
    }
}

/// Resolves roots, the workspace, and the layered configuration.
fn assemble_startup(
    factory: &ProductFactory,
    cli: &cli::Cli,
    vars: VarsMap,
    cwd: PathBuf,
) -> Result<Startup, ExitCode> {
    let roots = match edge::resolve_roots(&vars, factory.binary) {
        Ok(roots) => roots,
        Err(error) => return Err(roots_exit(&vars, factory.binary, error)),
    };
    let workspace_path = edge::resolve_workspace_path(&cwd, cli.cd.as_deref());
    let workspace = match edge::validate_workspace(workspace_path.clone()) {
        Ok(workspace) => workspace,
        Err(edge::EdgeError::Io { path, source, .. }) => {
            return Err(two_lines(
                cli::texts::prompt_file_unreadable(&path, &source.to_string()),
                exit::ExitKind::RequestedFailure,
            ));
        }
        Err(_) => {
            return Err(two_lines(
                cli::texts::workspace_not_usable(&workspace_path.display().to_string()),
                exit::ExitKind::RequestedFailure,
            ));
        }
    };
    let config_path = edge::config_file_path(&roots);
    let user_toml = match edge::read_user_config(&config_path) {
        Ok(user) => user,
        Err(edge::EdgeError::Io {
            operation,
            path,
            source,
        }) => {
            return Err(two_lines(
                cli::texts::startup_file_failed(operation, &path, &source.to_string()),
                exit::ExitKind::RequestedFailure,
            ));
        }
        Err(_) => {
            let log_path = edge::log_file_path(&vars, factory.binary);
            return Err(two_lines(
                cli::texts::internal_error_at(
                    "edge",
                    "the user configuration could not be read",
                    &log_path,
                ),
                exit::ExitKind::Internal,
            ));
        }
    };
    let base = match Config::load(
        factory.config_product,
        &roots.data,
        factory.defaults,
        user_toml.as_deref(),
    ) {
        Ok(config) => config,
        Err(error) => {
            return Err(config_exit(
                factory.binary,
                &error,
                &edge::data_root_log_path(&roots.data),
            ));
        }
    };
    let (mode, model) = split_mode_model(cli.mode, cli.model.clone());
    let config = base.with_overrides(&ConfigOverrides {
        model: model.map(Into::into),
        mode,
        thinking: cli.thinking.map(map_thinking),
        approval: cli.approval.map(map_approval),
        screen: cli.screen.map(map_screen),
        sandbox: cli.sandbox.then_some(true),
    });
    let startup = Startup {
        vars,
        cwd,
        workspace_path,
        workspace,
        config,
        config_path,
        data_root: roots.data,
        helper: edge::current_exe(),
        serve_bind_explicit: user_sets_serve_bind(user_toml.as_deref()),
    };
    Ok(startup)
}

/// Reports whether the user configuration text sets `[serve] bind`.
///
/// `Config::load` has already accepted the text, so a parse failure here
/// cannot happen for text that reached this point; it reads as unset.
fn user_sets_serve_bind(user_toml: Option<&str>) -> bool {
    user_toml
        .and_then(|text| toml::from_str::<toml::Table>(text).ok())
        .and_then(|table| table.get("serve")?.as_table()?.get("bind").cloned())
        .is_some()
}

/// Renders a plugin load failure with the plugin's name, or `None` when the
/// error carries no plugin to name.
///
/// A configured plugin with no directory is the missing-plugin error. Every
/// other failure names the plugin whose directory holds the failing file and
/// keeps the loader's own `path:line:col: message` detail.
fn plugin_load_lines(
    error: &(dyn std::error::Error + Send + Sync + 'static),
    plugins_dir: &Path,
) -> Option<[String; 2]> {
    use dal_star::LoadError;

    let error = error.downcast_ref::<LoadError>()?;
    if let LoadError::UnknownPlugin { name, dir } = error {
        return Some(cli::texts::plugin_missing(name, &dir.join(&**name)));
    }
    let name = failing_plugin(error, plugins_dir)?;
    Some(cli::texts::plugin_failed_to_load(&name, &error.render()))
}

/// Names the plugin whose directory holds the file a load error points at.
fn failing_plugin(error: &dal_star::LoadError, plugins_dir: &Path) -> Option<String> {
    use dal_star::LoadError;

    let path = match error {
        LoadError::Eval { path, .. }
        | LoadError::Registration { path, .. }
        | LoadError::PluginRebound { path, .. }
        | LoadError::UnsupportedEntry { path }
        | LoadError::InvalidPlugin { path, .. }
        | LoadError::Uses { path, .. }
        | LoadError::Schema { path, .. }
        | LoadError::Asset { path, .. }
        | LoadError::Unreadable { path, .. } => path,
        LoadError::NameMismatch { dir, .. } => return Some(dir.to_string()),
        LoadError::MissingEntryFile { dir } => return Some(dir.file_name()?.to_str()?.to_owned()),
        _ => return None,
    };
    let relative = path.strip_prefix(plugins_dir).ok()?;
    Some(
        relative
            .components()
            .next()?
            .as_os_str()
            .to_str()?
            .to_owned(),
    )
}

/// Maps an `resolve_roots` failure to its usage diagnostics.
fn roots_exit(vars: &VarsMap, binary: &str, error: edge::EdgeError) -> ExitCode {
    match error {
        edge::EdgeError::HomeMissing => {
            let what = if cfg!(windows) {
                cli::texts::HOME_MISSING_WINDOWS
            } else {
                cli::texts::HOME_MISSING_POSIX
            };
            two_lines(
                [what.into(), cli::texts::HOME_MISSING_HINT.into()],
                exit::ExitKind::RequestedFailure,
            )
        }
        edge::EdgeError::InvalidProduct => {
            let log_path = edge::log_file_path(vars, binary);
            two_lines(
                cli::texts::internal_error_at("edge", "invalid product identity", &log_path),
                exit::ExitKind::Internal,
            )
        }
        edge::EdgeError::Workspace(path) => two_lines(
            cli::texts::workspace_not_usable(&path.display().to_string()),
            exit::ExitKind::RequestedFailure,
        ),
        edge::EdgeError::WorkspaceRelative => {
            let log_path = edge::log_file_path(vars, binary);
            two_lines(
                cli::texts::internal_error_at(
                    "edge",
                    "the workspace path is not absolute",
                    &log_path,
                ),
                exit::ExitKind::Internal,
            )
        }
        edge::EdgeError::Io { path, source, .. } => two_lines(
            cli::texts::prompt_file_unreadable(&path, &source.to_string()),
            exit::ExitKind::RequestedFailure,
        ),
    }
}

/// Calls the product factory exactly once and maps build failures to exits.
fn build_once(
    factory: &ProductFactory,
    data_root: PathBuf,
    config: &Config,
) -> Result<dal_agent::Product, ExitCode> {
    let plugins_dir = data_root.join("plugins");
    match (factory.build)(&BuildCx { data_root, config }) {
        Ok(product) => Ok(product),
        Err(BuildError::Registration(error)) => Err(two_lines(
            cli::texts::internal_error("product", &error.to_string()),
            exit::ExitKind::Internal,
        )),
        Err(BuildError::Section {
            ref section,
            ref source,
        }) if section.as_ref() == "plugins" => Err(two_lines(
            plugin_load_lines(&**source, &plugins_dir).unwrap_or_else(|| {
                [
                    format!("plugin load failed: {source}"),
                    cli::texts::CONFIG_FIX_HINT.into(),
                ]
            }),
            exit::ExitKind::RequestedFailure,
        )),
        Err(error) => Err(two_lines(
            [
                {
                    // Some product decoders already name the product.
                    let text = error.to_string();
                    if text.starts_with(&format!("{}: ", factory.binary)) {
                        text
                    } else {
                        format!("{}: {text}", factory.binary)
                    }
                },
                cli::texts::CONFIG_FIX_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        )),
    }
}

#[cfg(test)]
mod config_exit_tests {
    use super::config_error_lines;
    use dal_core::{ConfigError, ConfigProduct};
    use std::path::Path;

    /// Loads a one-key user config that must fail.
    fn load_error(user_toml: &str) -> ConfigError {
        dal_core::Config::load(ConfigProduct::Dalgon, Path::new("/t"), "", Some(user_toml))
            .unwrap_err()
    }

    /// Loads a one-key user config and renders its failure lines.
    fn lines_for(user_toml: &str) -> [String; 2] {
        config_error_lines(&load_error(user_toml), "dalgon")
    }

    #[test]
    fn near_miss_unknown_key_names_the_closest_key() {
        let lines = lines_for("modle = \"normal\"\n");
        assert_eq!(
            lines[0],
            format!(
                "dalgon: {}: unknown key \"modle\"",
                crate::edge::CONFIG_FILE_NAME
            )
        );
        assert_eq!(
            lines[1],
            "Did you mean \"mode\"? See dalgon docs dal://config."
        );
    }

    #[test]
    fn distant_unknown_key_lists_the_key_table_in_table_order() {
        let error = dal_core::Config::load(
            ConfigProduct::Dalgon,
            Path::new("/t"),
            "",
            Some("zzz = 1\n"),
        )
        .unwrap_err();
        let ConfigError::UnknownKey { known_keys, .. } = &error else {
            panic!("expected UnknownKey, got {error:?}");
        };
        let lines = config_error_lines(&error, "dalgon");
        assert_eq!(
            lines[0],
            format!(
                "dalgon: {}: unknown key \"zzz\"",
                crate::edge::CONFIG_FILE_NAME
            )
        );
        let expected = format!(
            "Known keys: {} See dalgon docs dal://config.",
            known_keys.join(", ")
        );
        assert_eq!(lines[1], expected, "the table follows the schema order");
    }

    #[test]
    fn subtable_unknown_keys_carry_their_suggestion() {
        let lines = lines_for("[rules]\nwach = true\n");
        assert_eq!(
            lines[0],
            format!(
                "dalgon: {}: unknown key \"rules.wach\"",
                crate::edge::CONFIG_FILE_NAME
            )
        );
        assert_eq!(
            lines[1],
            "Did you mean \"rules.watch\"? See dalgon docs dal://config."
        );
    }

    #[test]
    fn invalid_rules_values_name_the_key_value_and_allowed_range() {
        let lines = lines_for("[rules]\nmax_retries = \"many\"\n");
        assert!(
            lines[0].starts_with("dalgon: dal.toml:")
                && lines[0].contains("rules.max_retries")
                && lines[0].contains("many"),
            "the diagnostic identifies the config key and rejected value: {}",
            lines[0]
        );
        assert!(lines[1].contains("whole number"), "{}", lines[1]);
        let bounds = lines[1]
            .split(|character: char| !character.is_ascii_digit())
            .filter(|token| !token.is_empty());
        assert_eq!(bounds.collect::<Vec<_>>(), ["0", "20"], "{}", lines[1]);
    }

    #[test]
    fn syntax_failures_name_the_file_and_the_parser_line() {
        let lines = lines_for("mode = \"normal\"\nmode = \"eval-first\"\n");
        let prefix = format!("dalgon: {}:2: ", crate::edge::CONFIG_FILE_NAME);
        assert!(
            lines[0].starts_with(&prefix) && lines[0].len() > prefix.len(),
            "the first line carries the file, the line, and the parser message: {}",
            lines[0]
        );
        assert_eq!(lines[1], crate::cli::texts::CONFIG_FIX_HINT);
    }

    #[test]
    fn non_table_judge_uses_the_judge_lines() {
        let lines = lines_for("judge = 3\n");
        assert_eq!(
            lines[0],
            format!(
                "dalgon: {}: judge must be a table",
                crate::edge::CONFIG_FILE_NAME
            )
        );
        assert!(lines[1].starts_with("Use a [judge] table"), "{}", lines[1]);
    }

    #[test]
    fn invalid_eval_uses_render_for_the_dalgona_product() {
        let error = dal_core::Config::load(
            ConfigProduct::Dalgona,
            Path::new("/t"),
            "",
            Some("[eval]\nuses = [\"nope\"]\n"),
        )
        .unwrap_err();
        let lines = config_error_lines(&error, "dalgona");
        assert_eq!(lines[0], "dalgona: dal.toml: eval.uses \"nope\" is invalid");
        assert!(
            lines[1].starts_with("Use an exact operation id"),
            "{}",
            lines[1]
        );
    }

    /// An invalid `[rules]` value renders as exactly two lines: the key and
    /// the value in TOML form, then the fix. A string keeps its quotes, so a
    /// quoted number is not reported as if the number itself were out of
    /// range, and a number stays bare. It never falls back to the generic
    /// text and hint.
    #[test]
    fn rules_values_keep_their_rules_lines() {
        for (body, first, second) in [
            (
                "max_retries = \"3\"",
                "dalgon: dal.toml: rules.max_retries \"3\" is invalid",
                "Use a whole number from 0 to 20.",
            ),
            (
                "max_retries = 50",
                "dalgon: dal.toml: rules.max_retries 50 is invalid",
                "Use a whole number from 0 to 20.",
            ),
        ] {
            let lines = lines_for(&format!("[rules]\n{body}\n"));
            assert_eq!(lines, [first.to_owned(), second.to_owned()], "{body}");
        }
    }

    #[test]
    fn non_rules_invalid_values_keep_the_generic_fallback() {
        let lines = lines_for("mode = \"jit\"\n");
        assert_eq!(
            lines[0],
            "dalgon: invalid config value jit for mode: normal, eval-first, eval-only"
        );
    }
}

#[cfg(test)]
mod plugin_load_tests {
    use super::{BuildCx, BuildError, Config, plugin_load_lines};
    use dal_core::ConfigProduct;

    /// Builds the dal product over a temporary data root holding `files`
    /// under `plugins/`, and renders the plugin failure it reports.
    fn failure_lines(plugins_key: &str, files: &[(&str, &str)]) -> [String; 2] {
        let dir = tempfile::tempdir().expect("temporary data root");
        for (path, text) in files {
            let full = dir.path().join("plugins").join(path);
            std::fs::create_dir_all(full.parent().expect("a file path")).expect("plugin dir");
            std::fs::write(full, text).expect("plugin file");
        }
        let config = Config::load(
            ConfigProduct::Dalgon,
            dir.path(),
            "",
            Some(&format!("plugins = {plugins_key}\n")),
        )
        .expect("the configuration is valid");
        let built = crate::product::build(&BuildCx {
            data_root: dir.path().to_path_buf(),
            config: &config,
        });
        let Err(BuildError::Section { source, .. }) = built else {
            panic!("the plugin set must fail to load");
        };
        plugin_load_lines(&*source, &dir.path().join("plugins"))
            .expect("a plugin failure renders as plugin lines")
    }

    #[test]
    fn a_configured_plugin_without_a_directory_is_missing() {
        let lines = failure_lines("[\"gone\"]", &[]);
        let prefix = "dalgon: plugin \"gone\" is missing: ";
        let location = lines[0]
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("the missing-plugin prefix: {}", lines[0]));
        assert!(
            std::path::Path::new(location).ends_with("plugins/gone"),
            "the path names the plugin directory: {location}"
        );
        assert_eq!(
            lines[1],
            "Remove \"gone\" from the plugins key in dal.toml, or create that directory with a plugin.star file."
        );
    }

    #[test]
    fn an_evaluation_failure_names_the_plugin_and_its_location() {
        let lines = failure_lines(
            "[\"bad\"]",
            &[(
                "bad/plugin.star",
                "load(\"@dal/v1\", \"dal\")\n\nx = 1 + \"a\"\n",
            )],
        );
        assert!(
            lines[0].starts_with("dalgon: plugin \"bad\" failed to load: ")
                && lines[0].contains("plugin.star:3:"),
            "{}",
            lines[0]
        );
        assert_eq!(
            lines[1],
            "Fix the error and run dalgon again, or remove \"bad\" from the plugins key."
        );
    }
}

#[cfg(test)]
mod serve_bind_provenance_tests {
    use super::user_sets_serve_bind;

    #[test]
    fn only_a_user_set_bind_counts_as_explicit() {
        assert!(!user_sets_serve_bind(None));
        assert!(!user_sets_serve_bind(Some("")));
        assert!(!user_sets_serve_bind(Some("[serve]\nport = 7500\n")));
        assert!(!user_sets_serve_bind(Some("bind = \"0.0.0.0\"\n")));
        assert!(user_sets_serve_bind(Some(
            "[serve]\nbind = \"127.0.0.1\"\n"
        )));
        assert!(user_sets_serve_bind(Some("serve.bind = \"::1\"\n")));
    }
}
