//! Interactive terminal dispatch over the local or remote host contract.

use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dal_agent::{Env, Product};
use dal_core::Screen as ConfigScreen;
use dal_core::{Config, Workspace};
use dal_tui::{EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};
use dal_wire::{RemoteEndpoint, RemoteHost};

use crate::cli::{self, ColorArg};
use crate::edge;
use crate::exit;
use crate::{ProductFactory, Startup, VarsMap, session_ref, two_lines};

mod remote;
mod settings;
mod signal;

/// Guards the interactive path on a usable terminal; returns the failure
/// exit when any check fails.
fn terminal_gate(snapshot: &edge::TerminalSnapshot, term: Option<&str>) -> Option<ExitCode> {
    if !snapshot.stdin_tty || !snapshot.stdout_tty {
        return Some(two_lines(
            [
                cli::texts::NO_PROMPT.into(),
                cli::texts::NO_PROMPT_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        ));
    }
    if term.is_none_or(|value| value == "dumb") {
        return Some(two_lines(
            cli::texts::term_not_addressable(term.unwrap_or("unset")),
            exit::ExitKind::RequestedFailure,
        ));
    }
    let width = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    if width < 40 {
        return Some(two_lines(
            cli::texts::terminal_too_narrow(width),
            exit::ExitKind::RequestedFailure,
        ));
    }
    None
}

/// Builds the TUI options: terminal env facts, then the session
/// options carrying them.
fn build_opts(
    cli: &cli::Cli,
    workspace: Workspace,
    config: &Config,
    vars: &VarsMap,
    snapshot: &edge::TerminalSnapshot,
    no_color: bool,
    binary: &'static str,
) -> TuiOptions {
    let env = EnvFacts {
        stdin_tty: snapshot.stdin_tty,
        path: vars.get(OsStr::new("PATH")).cloned(),
        fonts: dal_tui::diagram::FontSnapshot::capture(),
        home: owned(vars, "HOME")
            .or_else(|| owned(vars, "USERPROFILE"))
            .map(|home| {
                std::fs::canonicalize(&home)
                    .map(|resolved| resolved.display().to_string())
                    .unwrap_or(home)
            }),
        term: owned(vars, "TERM"),
        term_program: owned(vars, "TERM_PROGRAM"),
        colorterm: owned(vars, "COLORTERM"),
        colorfgbg: owned(vars, "COLORFGBG"),
        wt_session: owned(vars, "WT_SESSION"),
        wt_version: owned(vars, "WT_VERSION"),
        multiplexer: dal_tui::MultiplexerFacts {
            tmux: vars.contains_key(OsStr::new("TMUX")),
            sty: vars.contains_key(OsStr::new("STY")),
            zellij: vars.contains_key(OsStr::new("ZELLIJ")),
        },
        width_mode: WidthMode::from_locale([
            captured(vars, "LC_ALL").unwrap_or(""),
            captured(vars, "LC_CTYPE").unwrap_or(""),
            captured(vars, "LANG").unwrap_or(""),
        ]),
        no_motion: vars.contains_key(OsStr::new("DAL_NO_MOTION")),
        debug: vars.contains_key(OsStr::new("DAL_DEBUG")),
    };
    TuiOptions {
        session: session_ref(cli, workspace),
        screen: match config.screen() {
            ConfigScreen::Inline => Screen::Inline,
            ConfigScreen::Fullscreen => Screen::Fullscreen,
        },
        default_model: config
            .model()
            .filter(|model| !model.is_empty())
            .map(Into::into),
        theme_request: match config.theme() {
            "auto" => ThemeRequest::Auto,
            "palette" => ThemeRequest::Palette,
            name => ThemeRequest::Named(name.into()),
        },
        images: config.images(),
        diagrams: config.tui().diagrams,
        motion: config.motion() && !env.no_motion,
        binary,
        editor: captured(vars, "VISUAL")
            .or_else(|| captured(vars, "EDITOR"))
            .filter(|value| !value.is_empty())
            .unwrap_or(if cfg!(windows) { "notepad" } else { "vi" })
            .into(),
        color: dal_tui::term::color_mode(&env, no_color),
        env,
        rt: tokio::runtime::Handle::current(),
    }
}

/// The diagram writer and the product settings editor the TUI saves through.
type SettingsWriters = (
    Box<dyn FnMut(bool) -> Result<(), TuiError> + Send>,
    dal_tui::ProductSettings<
        Box<dyn FnMut() -> Result<Vec<dal_core::ProductSetting>, TuiError> + Send>,
        Box<dyn FnMut(&dal_core::SettingEdit) -> Result<(), TuiError> + Send>,
    >,
);

/// Builds the `dal.toml` writers over one editor.
fn settings_writers(editor: settings::ConfigEditor, config: Config) -> SettingsWriters {
    let editor = std::sync::Arc::new(editor);
    let diagrams = {
        let editor = std::sync::Arc::clone(&editor);
        let mut config = config;
        move |enabled| editor.save_diagrams(&mut config, enabled)
    };
    let catalog = {
        let editor = std::sync::Arc::clone(&editor);
        move || editor.catalog()
    };
    let edit = move |edit: &dal_core::SettingEdit| editor.edit(edit);
    (
        Box::new(diagrams),
        dal_tui::ProductSettings {
            catalog: Box::new(catalog),
            edit: Box::new(edit),
        },
    )
}

/// Connects the TUI to a remote host and runs it.
async fn connect_remote(
    cli: &cli::Cli,
    addr: &str,
    cwd: &Path,
    opts: TuiOptions,
    writers: SettingsWriters,
) -> ExitCode {
    let endpoint = match endpoint(addr) {
        Ok(endpoint) => endpoint,
        Err(error) => return tui_error(error),
    };
    let host = if let Some(path) = connect_auth_path(&endpoint, cli.connect_token_file.as_deref()) {
        let token = match connect_token(path, cwd) {
            Ok(token) => token,
            Err(error) => return tui_error(error),
        };
        match RemoteHost::connect_with_auth(endpoint, &token).await {
            Ok(host) => host,
            Err(error) => return tui_error(TuiError::Backend(Box::new(error))),
        }
    } else {
        match RemoteHost::connect(endpoint).await {
            Ok(host) => host,
            Err(error) => return tui_error(TuiError::Backend(Box::new(error))),
        }
    };
    let model_host = host.clone();
    let model_rt = opts.rt.clone();
    let model_source = move || -> Result<Vec<dal_tui::picker::ModelOption>, TuiError> {
        let models = model_rt
            .block_on(model_host.models())
            .map_err(|error| TuiError::Backend(Box::new(error)))?;
        Ok(remote_model_options(models))
    };
    let (code, _shutdown) =
        run_blocking(remote::RemoteBackend(host), opts, model_source, writers).await;
    code
}
/// Starts the interactive client after the process edge has resolved all inputs.
pub(crate) async fn interactive(
    cli: &cli::Cli,
    startup: Startup,
    factory: ProductFactory,
    product: Product,
) -> ExitCode {
    let t0 = std::time::Instant::now();
    let snapshot = edge::terminal_snapshot();
    let term = captured(&startup.vars, "TERM");
    if let Some(code) = terminal_gate(&snapshot, term) {
        return code;
    }

    let Startup {
        vars,
        cwd,
        workspace_path: _,
        workspace,
        config,
        config_path,
        data_root,
        helper,
        ..
    } = startup;
    let no_color = edge::resolve_color(cli.color, &vars, snapshot.stdout_tty) == ColorArg::Never;
    if vars.contains_key(OsStr::new("DAL_DEBUG")) {
        eprintln!("[t1] edge {}ms", t0.elapsed().as_millis());
    }
    let opts = build_opts(
        cli,
        workspace,
        &config,
        &vars,
        &snapshot,
        no_color,
        factory.binary,
    );
    let editor =
        settings::ConfigEditor::new(factory, &config, data_root.clone(), config_path.clone());
    if let Some(addr) = cli.connect.as_deref() {
        let binary = factory.binary;
        let (diagrams, mut product) = settings_writers(editor, config);
        // Local product rows would not describe what a remote host runs, and
        // a local `dal.toml` edit never changes it: the remote session lists
        // no product rows and refuses any product edit. Only the local
        // diagram preference is saved here.
        product.catalog = Box::new(|| Ok(Vec::new()));
        product.edit = Box::new(move |_: &dal_core::SettingEdit| {
            Err(TuiError::Terminal(format!(
                "{binary}: product settings belong to the host process; edit its dal.toml and restart it"
            )))
        });
        return connect_remote(cli, addr, &cwd, opts, (diagrams, product)).await;
    }
    let writers = settings_writers(editor, config.clone());
    let dal_debug = vars.contains_key(OsStr::new("DAL_DEBUG"));
    if dal_debug {
        eprintln!("[t1] pre-host {}ms", t0.elapsed().as_millis());
    }
    #[cfg(feature = "test-support")]
    let login_endpoints = match super::login::test_login_endpoints(&vars) {
        Ok(endpoints) => endpoints,
        Err(error) => return crate::host_exit(&dal_agent::HostError::Provider(error), &data_root),
    };
    let host = match super::start_host(
        product,
        config,
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
        &data_root,
        &config_path,
    )
    .await
    {
        Ok(host) => host,
        Err(code) => return code,
    };
    #[cfg(feature = "test-support")]
    if let Some(endpoints) = login_endpoints
        && let Err(error) = host.set_login_endpoints(endpoints)
    {
        let _ = host.shutdown(std::time::Duration::from_secs(3)).await;
        return crate::host_exit(&error, &data_root);
    }
    let shutdown = host.clone();
    let model_host = host.clone();
    let model_rt = opts.rt.clone();
    let model_source = move || -> Result<Vec<dal_tui::picker::ModelOption>, TuiError> {
        let models = model_rt.block_on(model_host.models(None))?;
        Ok(dal_tui::picker::model_options(models))
    };
    if dal_debug {
        eprintln!("[t1] host started {}ms", t0.elapsed().as_millis());
    }
    let (code, mut signals) = run_blocking(host, opts, model_source, writers).await;
    // The terminal is already restored here; the ladder closes the host.
    let closed = exit::Ladder::begin()
        .close_host(shutdown, &mut signals)
        .await;
    closed.skipped_by.map_or(code, ExitCode::from)
}

async fn run_blocking<H, M>(
    host: H,
    opts: TuiOptions,
    model_source: M,
    (save_diagrams, product): SettingsWriters,
) -> (ExitCode, edge::ShutdownSignals)
where
    H: dal_tui::backend::TuiHost,
    M: FnMut() -> Result<Vec<dal_tui::picker::ModelOption>, TuiError> + Send + 'static,
{
    let mut signals = signal::Signals::start();
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the process edge owns standard input"
    )]
    let io = signals.terminal(std::io::stdin());
    let mut task = tokio::task::spawn_blocking(move || {
        dal_tui::run_backend_with_product_settings(
            &host,
            &opts,
            &io,
            model_source,
            save_diagrams,
            product,
        )
    });
    let polled = tokio::select! {
        biased;
        _ = signals.await_shutdown() => None,
        result = &mut task => Some(result),
    };
    let result = match polled {
        Some(result) => result,
        None => (&mut task).await,
    };
    let signal = signals.exit_status();
    let shutdown = signals.into_shutdown();
    if let Some(code) = signal {
        return (ExitCode::from(code), shutdown);
    }
    let code = match result {
        Ok(Ok(_)) => exit::code(exit::ExitKind::Success),
        Ok(Err(error)) => tui_error(error),
        Err(error) => tui_error(TuiError::Terminal(format!(
            "dalgon: interactive terminal stopped: {error}\nRestart dalgon in a terminal."
        ))),
    };
    (code, shutdown)
}

fn tui_error(error: TuiError) -> ExitCode {
    match error {
        TuiError::Terminal(text) => {
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), text.as_bytes());
            let _ = std::io::Write::write_all(&mut std::io::stderr().lock(), b"\n");
            exit::code(exit::ExitKind::RequestedFailure)
        }
        error => two_lines(
            [
                format!("dalgon: {error}"),
                "Check the host and try again.".into(),
            ],
            exit::ExitKind::RequestedFailure,
        ),
    }
}

/// Maps the rows of a remote `models/list` to picker options.
///
/// A row whose server publishes a typed route selects by that route and
/// nothing else: the picker submits `SetModel` of the route exactly as the
/// local picker does, and the route's configured provider is the scope
/// identity. A row from a server that predates typed routes keeps its
/// original `/model <reference>` selection and has no scope identity: its
/// `provider` is only a family label, which cannot name a configured
/// provider. Rows that spell one label (an API model and a synthetic model
/// both named `openrouter/free`) stay separate rows and are told apart by
/// their route kind.
fn remote_model_options(
    models: Vec<dal_wire::remote::RemoteModel>,
) -> Vec<dal_tui::picker::ModelOption> {
    let (mut options, kinds): (Vec<_>, Vec<_>) =
        models.into_iter().map(remote_model_option).unzip();
    let mut spelled: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for option in &options {
        *spelled.entry(option.label.clone()).or_default() += 1;
    }
    for (option, kind) in options.iter_mut().zip(kinds) {
        if let Some(kind) = kind
            && spelled.get(&option.label).is_some_and(|count| *count > 1)
        {
            option.label = format!("{} ({kind})", option.label);
        }
    }
    options
}

/// One picker option, and the route kind that tells it apart from a row with
/// the same label; `None` for a row that carries no typed route.
fn remote_model_option(
    model: dal_wire::remote::RemoteModel,
) -> (dal_tui::picker::ModelOption, Option<&'static str>) {
    let Some(route) = model.route else {
        let reference = if dal_provider::find(&model.provider).is_some() {
            format!("{}/{}", model.provider, model.id)
        } else {
            model.id
        };
        let option = dal_tui::picker::ModelOption {
            label: format!("{} · {reference}", model.name),
            command: dal_core::Command::Run {
                name: "model".into(),
                args: reference.into(),
                expected: None,
            },
            scoped_id: None,
        };
        return (option, None);
    };
    let shown = model.reference.unwrap_or_else(|| route.id().to_owned());
    let (kind, scoped_id) = match &route {
        dal_core::ModelRoute::Api {
            provider: Some(provider),
            model: native,
            ..
        } if !provider.is_empty() => ("api", Some(format!("{provider}/{native}").into())),
        dal_core::ModelRoute::Api { .. } => ("api", None),
        dal_core::ModelRoute::Synthetic { .. } => ("synthetic", None),
        dal_core::ModelRoute::Harness { .. } => ("harness", None),
    };
    let option = dal_tui::picker::ModelOption {
        label: format!("{} · {shown}", model.name),
        command: dal_core::Command::SetModel {
            model: route,
            // A remote selection stays session-only: this terminal cannot
            // save a default into the remote host's own `dal.toml`.
            save: dal_core::command::Save::SessionOnly,
        },
        scoped_id,
    };
    (option, Some(kind))
}

fn endpoint(addr: &str) -> Result<RemoteEndpoint, TuiError> {
    if addr.starts_with('/') {
        return Ok(RemoteEndpoint::LocalSocket(PathBuf::from(addr)));
    }
    let url = if addr.starts_with("ws://") || addr.starts_with("wss://") {
        url::Url::parse(addr)
    } else {
        url::Url::parse(&format!("ws://{addr}/v1/ws"))
    }
    .map_err(|error| TuiError::Terminal(format!(
        "dalgon: --connect address is invalid: {error}\nUse a ws:// or wss:// URL, host:port, or absolute socket path."
    )))?;
    Ok(RemoteEndpoint::WebSocket(url))
}

fn is_loopback_websocket(url: &url::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let host = host.strip_prefix('[').unwrap_or(host);
        let host = host.strip_suffix(']').unwrap_or(host);
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn connect_auth_path<'a>(
    endpoint: &RemoteEndpoint,
    path: Option<&'a std::path::Path>,
) -> Option<&'a std::path::Path> {
    match (endpoint, path) {
        (RemoteEndpoint::WebSocket(url), Some(path)) if !is_loopback_websocket(url) => Some(path),
        _ => None,
    }
}

fn connect_token(path: &std::path::Path, cwd: &std::path::Path) -> Result<String, TuiError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    dal_wire::token::read_connect_token(&path).map_err(|error| {
        TuiError::Terminal(format!(
            "dalgon: cannot read connect token file {}: {error}\nCheck the file and try again.",
            path.display()
        ))
    })
}

fn captured<'a>(vars: &'a crate::VarsMap, name: &str) -> Option<&'a str> {
    vars.get(OsStr::new(name)).and_then(|value| value.to_str())
}

fn owned(vars: &crate::VarsMap, name: &str) -> Option<String> {
    captured(vars, name).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{connect_auth_path, connect_token, is_loopback_websocket};
    use dal_wire::RemoteEndpoint;

    fn websocket(address: &str) -> RemoteEndpoint {
        RemoteEndpoint::WebSocket(url::Url::parse(address).expect("valid test URL"))
    }

    #[test]
    fn connect_auth_requires_an_explicit_token_for_non_loopback_websockets() {
        let token = Path::new("connect.token");
        assert_eq!(
            connect_auth_path(&websocket("ws://localhost:1234"), Some(token)),
            None
        );
        assert_eq!(
            connect_auth_path(&websocket("ws://127.0.0.1:1234"), Some(token)),
            None
        );
        assert_eq!(
            connect_auth_path(&websocket("ws://[::1]:1234"), Some(token)),
            None
        );
        assert_eq!(
            connect_auth_path(&websocket("ws://example.com:1234"), None),
            None
        );
        assert_eq!(
            connect_auth_path(&websocket("wss://example.com:1234"), Some(token)),
            Some(token),
        );
        assert_eq!(
            connect_auth_path(
                &RemoteEndpoint::LocalSocket("/tmp/dalgon.sock".into()),
                Some(token),
            ),
            None,
        );
        assert!(is_loopback_websocket(
            &url::Url::parse("ws://127.8.9.10").expect("valid URL")
        ));
    }

    #[test]
    fn connect_token_reads_one_line_and_fails_closed_for_missing_or_empty_files() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("connect.token");
        let token = format!("dal_{}", "a".repeat(64));
        std::fs::write(&path, format!("{token}\r\nignored")).expect("write token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("secure token permissions");
        }
        assert_eq!(connect_token(&path, dir.path()).expect("read token"), token);
        assert_eq!(
            connect_token(Path::new("connect.token"), dir.path()).expect("resolve relative token"),
            token,
        );
        std::fs::write(&path, b"").expect("truncate token");
        assert!(
            connect_token(&path, dir.path())
                .is_err_and(|error| error.to_string().contains("empty"))
        );
        let missing = dir.path().join("missing.token");
        assert!(
            connect_token(&missing, dir.path())
                .is_err_and(|error| error.to_string().contains("cannot read"))
        );
        assert!(connect_token(dir.path(), dir.path()).is_err());
    }

    /// The remote model picker: each row of a server's `models/list` becomes
    /// one option that selects by the row's typed route and nothing else.
    mod remote_picker {
        use dal_core::command::Save;
        use dal_core::{Command, Family, ModelRoute};
        use dal_tui::picker::ModelOption;
        use dal_wire::remote::RemoteModel;

        use super::super::remote_model_options;

        fn old_row(id: &str, provider: &str, name: &str) -> RemoteModel {
            RemoteModel {
                id: id.into(),
                provider: provider.into(),
                name: name.into(),
                context_window: None,
                route: None,
                reference: None,
            }
        }

        fn typed_row(name: &str, route: ModelRoute, reference: Option<&str>) -> RemoteModel {
            RemoteModel {
                id: route.id().into(),
                provider: "openai".into(),
                name: name.into(),
                context_window: None,
                route: Some(route),
                reference: reference.map(str::to_owned),
            }
        }

        fn select(route: ModelRoute) -> Command {
            Command::SetModel {
                model: route,
                save: Save::SessionOnly,
            }
        }

        #[test]
        fn rows_from_a_server_without_typed_routes_keep_the_model_command_and_no_scope() {
            let route = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-4o".into(),
                provider: Some("acme-proxy".into()),
            };
            let options = remote_model_options(vec![
                old_row("gpt-4o", "openai", "GPT-4o"),
                old_row("acme/fast", "acme", "acme/fast"),
                typed_row(
                    "GPT-4o via acme-proxy",
                    route.clone(),
                    Some("acme-proxy/gpt-4o"),
                ),
                old_row("gpt-4o", "openai", "GPT-4o"),
            ]);
            // One old server row names a configured provider (`openai` is a
            // provider id as well as a family label), the other a synthetic
            // namespace; a row without a route never gets a scope identity,
            // and the list is not refused for lacking the members. Two
            // routeless rows with one label stay as they are: only typed rows
            // are told apart by kind.
            let run = |reference: &str| Command::Run {
                name: "model".into(),
                args: reference.into(),
                expected: None,
            };
            assert_eq!(
                options,
                vec![
                    ModelOption {
                        label: "GPT-4o · openai/gpt-4o".into(),
                        command: run("openai/gpt-4o"),
                        scoped_id: None,
                    },
                    ModelOption {
                        label: "acme/fast · acme/fast".into(),
                        command: run("acme/fast"),
                        scoped_id: None,
                    },
                    ModelOption {
                        label: "GPT-4o via acme-proxy · acme-proxy/gpt-4o".into(),
                        command: select(route),
                        scoped_id: Some("acme-proxy/gpt-4o".into()),
                    },
                    ModelOption {
                        label: "GPT-4o · openai/gpt-4o".into(),
                        command: run("openai/gpt-4o"),
                        scoped_id: None,
                    },
                ]
            );
        }

        #[test]
        fn rows_that_spell_one_label_are_told_apart_by_their_route_kind() {
            let api = ModelRoute::Api {
                family: Family::Responses,
                model: "free".into(),
                provider: Some("openrouter".into()),
            };
            let synthetic = ModelRoute::Synthetic {
                id: "openrouter/free".into(),
            };
            let harness = ModelRoute::Harness {
                id: "dalgon/normal".into(),
            };
            let unnamed = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-4o".into(),
                provider: None,
            };
            let options = remote_model_options(vec![
                typed_row("Free", api.clone(), Some("openrouter/free")),
                typed_row("Free", synthetic.clone(), Some("openrouter/free")),
                typed_row("Normal", harness.clone(), Some("dalgon/normal")),
                typed_row("Bare", unnamed.clone(), None),
            ]);
            let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
            assert_eq!(
                labels,
                [
                    "Free · openrouter/free (api)",
                    "Free · openrouter/free (synthetic)",
                    "Normal · dalgon/normal",
                    "Bare · gpt-4o",
                ]
            );
            let commands: Vec<Command> = options
                .iter()
                .map(|option| option.command.clone())
                .collect();
            assert_eq!(
                commands,
                [
                    select(api),
                    select(synthetic),
                    select(harness),
                    select(unnamed)
                ]
            );
            // Only an API row whose provider the server named has a scope id.
            let scoped: Vec<Option<&str>> = options
                .iter()
                .map(|option| option.scoped_id.as_deref())
                .collect();
            assert_eq!(scoped, [Some("openrouter/free"), None, None, None]);
        }
    }
}
