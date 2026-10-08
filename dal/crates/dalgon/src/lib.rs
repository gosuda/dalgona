//! The `dalgon` command-line and process edge for the dal product.
#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use dal_agent::{Env, Host, HostError, SessionRef};
use dal_core::{
    ApprovalMode, ClientId, ConfigOverrides, ConfigProduct, Mode, Screen, ThinkingLevel, Workspace,
};
use tokio::io::AsyncWriteExt as _;

/// Product configuration shared with the edge builder.
pub use dal_core::Config;
/// Typed product-configuration failures.
pub use dal_core::ConfigError;
/// Typed extension-registration failures.
pub use dal_core::RegistrationError;
/// One first-party documentation scheme and its pages.
pub use dal_ext::docs::Manual as ProductManual;

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
    /// Embedded product-default TOML layered after dal code defaults.
    pub defaults: &'static str,
    /// Builds the architecture product from the captured edge context.
    pub build: fn(&BuildCx<'_>) -> Result<dal_agent::Product, BuildError>,
    /// Returns every first-party manual, without loading user plugins.
    pub docs: fn() -> Vec<ProductManual>,
}

/// The single Clap command tree shared by parsing, help, completion, and man pages.
pub mod cli;
/// Command dispatch over the built product.
mod dispatch;
/// Process-edge snapshots, lexical paths, and platform identity.
mod edge;
/// Exit-code mapping for command and signal outcomes.
mod exit;
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
    pub(crate) config_path: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) helper: Option<PathBuf>,
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
    let cli = match cli::parse_cli(factory.binary, argv) {
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
    let startup = match assemble_startup(&factory, &cli, vars, cwd) {
        Ok(startup) => startup,
        Err(code) => return code,
    };
    if let Some(cli::Commands::Docs(args)) = &cli.command {
        return dispatch::docs(args.clone(), factory.binary, (factory.docs)());
    }
    let product = match build_once(&factory, startup.data_root.clone(), &startup.config) {
        Ok(product) => product,
        Err(code) => return code,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(64)
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let log_path = edge::log_file_path(&startup.vars, factory.binary);
            return two_lines(
                cli::texts::internal_error_at(
                    "edge",
                    &format!("cannot start the runtime: {error}"),
                    &log_path,
                ),
                exit::ExitKind::Internal,
            );
        }
    };
    let code = runtime.block_on(run_command(factory, cli, startup, product));
    runtime.shutdown_timeout(Duration::from_millis(500));
    code
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
                config_path: _,
                data_root,
                helper,
            } = startup;
            run_headless(
                &cli,
                vars,
                cwd,
                workspace_path,
                workspace,
                config,
                product,
                helper,
                &data_root,
            )
            .await
        }
        #[cfg(feature = "tui")]
        None => Box::pin(dispatch::interactive(&cli, startup, product)).await,
        #[cfg(not(feature = "tui"))]
        None => two_lines(
            [
                cli::texts::NO_PROMPT.into(),
                cli::texts::NO_PROMPT_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
        Some(cli::Commands::Completion(args)) => {
            let script = cli::completion_script(args.shell, factory.binary);
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
        Some(cli::Commands::Login(args)) => dispatch::login(args.clone(), startup).await,
        Some(cli::Commands::Logout(args)) => dispatch::logout(args.clone(), startup).await,
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
#[expect(
    clippy::too_many_arguments,
    reason = "one headless run carries cli, vars, paths, and host state"
)]
async fn run_headless(
    cli: &cli::Cli,
    vars: VarsMap,
    cwd: PathBuf,
    workspace_path: PathBuf,
    workspace: Workspace,
    config: Config,
    product: dal_agent::Product,
    helper: Option<PathBuf>,
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
    let headless_approval = config.approval() == dal_core::ApprovalMode::Ask;
    let session = session_ref(cli, workspace);
    let host = match Host::start(
        product,
        config,
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
    )
    .await
    {
        Ok(host) => host,
        Err(error) => return host_exit(&error, data_root),
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
        stderr_is_tty: snapshot.stderr_tty,
        headless_approval,
        stop: stop.clone(),
        quiet_wait: (!cli.json).then_some(print::QUIET_WAIT),
    };
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let outcome = dispatch::drive(
        &stop,
        print::run_print(agent, options, &mut stdout, &mut stderr),
    )
    .await;
    let shutdown = if matches!(&outcome, Ok(Err(print::PrintError::NotQuiet { .. }))) {
        host.shutdown_after_quiet_wait(Duration::from_secs(3)).await
    } else {
        host.shutdown(Duration::from_secs(3)).await
    };
    if !shutdown.status_quiet && !matches!(&outcome, Ok(Err(print::PrintError::NotQuiet { .. }))) {
        let [what, hint] = cli::texts::status_not_quiet_shutdown();
        let _ = stderr
            .write_all(format!("{what}\n{hint}\n").as_bytes())
            .await;
    }
    match outcome {
        Ok(Ok(print::PrintOutcome::Completed)) => exit::code(exit::ExitKind::Success),
        Ok(Ok(print::PrintOutcome::Interrupted)) => exit::code(exit::ExitKind::Signal(2)),
        Ok(Err(error)) if error.is_broken_pipe() => exit::code(exit::ExitKind::Signal(13)),
        // A headless denial is a requested failure like a tool error.
        Ok(Ok(print::PrintOutcome::Denied) | Err(_)) => {
            exit::code(exit::ExitKind::RequestedFailure)
        }
        Err(code) => ExitCode::from(code),
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
        Some(id) if cli::is_mode_id(&id) => (Some(mode_for_id(&id)), None),
        model => (None, model),
    }
}

/// Maps the mode-form model id to its harness mode.
fn mode_for_id(id: &str) -> Mode {
    match id {
        "dalgon/normal" => Mode::Normal,
        "dalgon/eval-first" => Mode::EvalFirst,
        "dalgon/eval-only" => Mode::EvalOnly,
        _ => {
            unreachable!("mode-form ids are closed by cli::is_mode_id")
        }
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
fn config_exit(binary: &str, error: &ConfigError) -> ExitCode {
    if let Some([first, second]) = error.rules_lines(binary) {
        return two_lines([first, second], exit::ExitKind::RequestedFailure);
    }
    two_lines(
        [
            format!("{binary}: {error}"),
            cli::texts::CONFIG_FIX_HINT.into(),
        ],
        exit::ExitKind::RequestedFailure,
    )
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
        Err(edge::EdgeError::Io { path, source, .. }) => {
            return Err(two_lines(
                cli::texts::prompt_file_unreadable(&path, &source.to_string()),
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
    let kind = if factory.binary == "dalgona" {
        ConfigProduct::Dalgona
    } else {
        ConfigProduct::Dalgon
    };
    let base = match Config::load(kind, &roots.data, factory.defaults, user_toml.as_deref()) {
        Ok(config) => config,
        Err(error) => return Err(config_exit(factory.binary, &error)),
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
    };
    Ok(startup)
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
            [
                format!("plugin load failed: {source}"),
                cli::texts::CONFIG_FIX_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        )),
        Err(error) => Err(two_lines(
            [
                format!("dalgon: {error}"),
                cli::texts::CONFIG_FIX_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        )),
    }
}
