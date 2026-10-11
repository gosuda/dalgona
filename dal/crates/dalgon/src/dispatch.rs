//! Command dispatch over the built product.
//!
//! Each arm adapts one CLI subcommand to its handler module. Handlers stay
//! in their modules; this layer only maps arguments, builds typed inputs,
//! and renders typed errors to their exit codes.

use std::io::Write;
use std::process::ExitCode;

use dal_agent::{Env, GrantStore, Host, Product};
use dal_core::{ClientId, Config, Name, Origin};
use tokio_util::sync::CancellationToken;

type VarsMap = std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>;

use crate::cli;
use crate::edge;
use crate::exit;
use crate::plugin_cmd::{self, ConfiguredPlugin, PluginCommand};
use crate::rules_cmd::{self, RuleTestArgs, RuleTestSource, RulesCommand};
use crate::serve;
use crate::{Startup, host_exit, map_approval, two_lines};

#[cfg(feature = "tui")]
mod interactive;
mod wires;

#[cfg(feature = "tui")]
pub(crate) use interactive::interactive;

pub(crate) use wires::{acp, app_server, rpc};
mod dev;
mod docs;
mod login;
mod logout;
mod models;

pub(crate) use dev::run as dev;
pub(crate) use docs::run as docs;
pub(crate) use login::run as login;
pub(crate) use logout::run as logout;
pub(crate) use models::run as models;

/// Drives `future` until it completes, cancelling `stop` on the first
/// shutdown signal and abandoning the wait on the second.
///
/// Returns the inner output, or the `128 + signo` code when a second signal
/// arrives while draining. Dropping the future closes the listener without
/// waiting out the grace period.
pub(crate) async fn drive<Fut>(stop: &CancellationToken, future: Fut) -> Result<Fut::Output, u8>
where
    Fut: Future,
{
    tokio::pin!(future);
    let mut signalled = false;
    loop {
        tokio::select! {
            biased;
            done = &mut future => return Ok(done),
            code = edge::shutdown_signal() => {
                if signalled {
                    return Err(code);
                }
                signalled = true;
                stop.cancel();
            }
        }
    }
}

/// Renders one serve failure that escaped the in-command diagnostics.
fn serve_error(error: serve::ServeCommandError) -> ExitCode {
    match error {
        serve::ServeCommandError::Io(source) => two_lines(
            [
                format!("dalgon: cannot write serve output: {source}"),
                "Check that the output stream is open, then try again.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
        error => two_lines(
            crate::cli::texts::internal_error("serve", &error.to_string()),
            exit::ExitKind::Internal,
        ),
    }
}

/// Runs `dalgon serve` or `dalgon serve token` over the built product.
pub(crate) async fn serve(
    cli: &cli::Cli,
    args: cli::ServeCliArgs,
    startup: Startup,
    product: Product,
) -> ExitCode {
    let Startup {
        vars,
        cwd,
        workspace_path,
        workspace: _,
        config,
        config_path: _,
        data_root,
        binary: _,
        helper,
    } = startup;
    let core_serve = config.serve();
    if let Some(cli::ServeSubcommand::Token(token)) = &args.command {
        let token_file = args
            .token_file
            .unwrap_or_else(|| core_serve.token_file.clone());
        let snapshot = edge::terminal_snapshot();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let stderr = std::io::stderr();
        let mut err = stderr.lock();
        return match serve::create_token(
            &token_file,
            token.force,
            snapshot.stderr_tty,
            &mut out,
            &mut err,
        )
        .await
        {
            Ok(code) => code,
            Err(error) => serve_error(error),
        };
    }
    let serve_config = serve::ServeConfig {
        bind: core_serve.bind.to_string(),
        bind_was_explicit: false,
        port: core_serve.port,
        token_file: core_serve.token_file.clone(),
        approval: cli.approval.map_or(core_serve.approval, map_approval),
        origins: core_serve
            .origins
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
        aliases: config.aliases().clone(),
        workspace: workspace_path,
    };
    let serve_args = serve::ServeArgs {
        public: args.public,
        a2a: args.a2a,
        bind: args.bind,
        port: args.port,
        token_file: args.token_file,
    };
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
        Err(error) => return host_exit(&error, &data_root),
    };
    let stop = CancellationToken::new();
    let snapshot = edge::terminal_snapshot();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    match drive(
        &stop,
        serve::run(
            host,
            serve_args,
            serve_config,
            data_root,
            stop.clone(),
            &mut out,
            &mut err,
            snapshot.stderr_tty,
            snapshot.stdout_tty,
        ),
    )
    .await
    {
        Ok(Ok(code)) => code,
        Ok(Err(error)) => serve_error(error),
        Err(code) => ExitCode::from(code),
    }
}

/// Writes the unconfigured-plugin diagnostic for `name`.
fn plugin_not_configured(name: &str) -> ExitCode {
    let _ = writeln!(
        std::io::stderr().lock(),
        "{}",
        crate::cli::texts::plugin_not_configured(name)
    );
    exit::code(exit::ExitKind::RequestedFailure)
}

/// Renders one plugin-command failure with its exit status.
fn plugin_error(name: &str, error: plugin_cmd::PluginCommandError) -> ExitCode {
    match error {
        plugin_cmd::PluginCommandError::Io(source) => {
            let _ = writeln!(std::io::stderr().lock(), "{source}");
            exit::code(exit::ExitKind::RequestedFailure)
        }
        plugin_cmd::PluginCommandError::Store(source) => two_lines(
            [
                format!("dalgon: cannot read the grants file: {source}"),
                "Fix the permissions on the grants file, or point the XDG variables at a writable location.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
        plugin_cmd::PluginCommandError::Builtin => two_lines(
            [
                format!("dalgon: plugin \"{name}\" is a builtin extension: it needs no grant"),
                "Grant only user or bundled plugins.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
    }
}

/// Runs `dalgon plugin grant|revoke|list` over the loaded declarations.
pub(crate) async fn plugin(args: cli::PluginArgs, startup: Startup, product: Product) -> ExitCode {
    let data_root = startup.data_root.clone();
    let plugins: Vec<ConfiguredPlugin> = product
        .extensions
        .iter()
        .filter_map(ConfiguredPlugin::from_extension)
        .collect();
    let grants = GrantStore::new(data_root);
    let by = ClientId::new("cli");
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match args.command {
        cli::PluginSubcommand::Grant { name } => {
            let parsed: Name = match name.parse() {
                Ok(name) => name,
                Err(_) => return plugin_not_configured(&name),
            };
            if product
                .extensions
                .iter()
                .any(|extension| extension.name() == name && extension.origin() == Origin::Builtin)
            {
                return plugin_error(&name, plugin_cmd::PluginCommandError::Builtin);
            }
            match plugin_cmd::run(
                PluginCommand::Grant { name: parsed },
                &plugins,
                &grants,
                by,
                &mut out,
            )
            .await
            {
                Ok(code) => code,
                Err(error) => plugin_error(&name, error),
            }
        }
        cli::PluginSubcommand::Revoke { name } => {
            let parsed: Name = match name.parse() {
                Ok(name) => name,
                Err(_) => return plugin_not_configured(&name),
            };
            match plugin_cmd::run(
                PluginCommand::Revoke { name: parsed },
                &plugins,
                &grants,
                by,
                &mut out,
            )
            .await
            {
                Ok(code) => code,
                Err(error) => plugin_error(&name, error),
            }
        }
        cli::PluginSubcommand::List => {
            match plugin_cmd::run(PluginCommand::List, &plugins, &grants, by, &mut out).await {
                Ok(code) => code,
                Err(error) => plugin_error("", error),
            }
        }
    }
}

/// Renders one rules-command failure with its exit status.
fn rules_error(error: &rules_cmd::RulesCommandError) -> ExitCode {
    let code = error.exit_code();
    let _ = writeln!(std::io::stderr().lock(), "{error}");
    code
}

/// Runs `dalgon rules` or `dalgon rules test` over the TTSR engine.
#[expect(
    clippy::disallowed_methods,
    reason = "R4 edge: the rules wire owns standard input"
)]
fn rules_stdin() -> tokio::io::Stdin {
    tokio::io::stdin()
}

pub(crate) async fn rules(args: cli::RulesArgs, startup: Startup, product: Product) -> ExitCode {
    let command = match args.command {
        None => RulesCommand::List,
        Some(cli::RulesSubcommand::Test(test)) => {
            if test.source != cli::RuleTestSourceArg::Tool
                && (test.tool.is_some() || test.path.is_some())
            {
                return two_lines(
                    [
                        "dalgon: --tool and --path require --source tool".into(),
                        "Pass --source tool, or omit --tool and --path.".into(),
                    ],
                    exit::ExitKind::Usage,
                );
            }
            RulesCommand::Test(RuleTestArgs {
                source: match test.source {
                    cli::RuleTestSourceArg::Text => RuleTestSource::Text,
                    cli::RuleTestSourceArg::Thinking => RuleTestSource::Thinking,
                    cli::RuleTestSourceArg::Tool => RuleTestSource::Tool,
                },
                tool: test.tool.unwrap_or_else(|| "patch".to_owned()),
                path: test.path,
                text: test.text,
            })
        }
    };
    let tool_names: Vec<String> = product
        .extensions
        .iter()
        .flat_map(|extension| {
            extension
                .tools()
                .iter()
                .map(|(tool, _)| tool.name().as_str().to_owned())
        })
        .collect();
    let known: Vec<&str> = tool_names.iter().map(String::as_str).collect();
    let input = dal_ext::ttsr::build::RuleBuildInput {
        records: &[],
        plugin_rules: &[],
        known_tools: &known,
        agent: "main",
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match rules_cmd::run(
        command,
        &input,
        &startup.data_root,
        &startup.workspace_path,
        startup.config.rules(),
        &mut rules_stdin(),
        &mut out,
    )
    .await
    {
        Ok(code) => code,
        Err(error) => rules_error(&error),
    }
}

/// Starts the host or renders its startup failure.
pub(crate) async fn start_host(
    product: Product,
    config: &Config,
    vars: VarsMap,
    cwd: std::path::PathBuf,
    helper: Option<std::path::PathBuf>,
    data_root: &std::path::Path,
) -> Result<Host, ExitCode> {
    match Host::start(
        product,
        config.clone(),
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
    )
    .await
    {
        Ok(host) => Ok(host),
        Err(error) => Err(host_exit(&error, data_root)),
    }
}
