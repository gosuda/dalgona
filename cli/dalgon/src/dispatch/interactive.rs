//! Interactive terminal dispatch over the local or remote host contract.

use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dal_agent::{DefaultAuthority, Env, Host, Product};
use dal_core::Screen as ConfigScreen;
use dal_core::{Config, Workspace};
use dal_tui::{EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};
use dal_wire::{RemoteEndpoint, RemoteHost};

use crate::cli::{self, ColorArg};
use crate::edge;
use crate::exit;
use crate::{Startup, VarsMap, host_exit, session_ref, two_lines};

mod remote;
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

/// Persists a TUI diagrams toggle back into dal.toml.
fn diagram_saver(
    mut config: Config,
    config_path: PathBuf,
) -> impl FnMut(bool) -> Result<(), TuiError> {
    let save_config_path = config_path;
    move |enabled| -> Result<(), TuiError> {
        let user_toml = edge::read_user_config(&save_config_path).map_err(|error| {
            TuiError::Terminal(format!("dalgon: cannot read dal.toml: {error}"))
        })?;
        let updated = config
            .update_tui_diagrams(enabled, user_toml.as_deref())
            .map_err(|error| {
                TuiError::Terminal(format!("dalgon: cannot update dal.toml: {error}"))
            })?;
        let config_dir = save_config_path.parent().ok_or_else(|| {
            TuiError::Terminal("dalgon: config path has no parent directory".to_owned())
        })?;
        dal_store::create_private_dir_all(config_dir).map_err(|error| {
            TuiError::Terminal(format!("dalgon: cannot create config directory: {error}"))
        })?;
        dal_store::write_atomic(
            &save_config_path,
            updated.as_bytes(),
            dal_store::FileMode::Mode0600,
        )
        .map_err(|error| {
            TuiError::Terminal(format!(
                "dalgon: cannot save {}: {error}",
                save_config_path.display()
            ))
        })?;
        Ok(())
    }
}

/// Connects the TUI to a remote host and runs it.
async fn connect_remote(
    cli: &cli::Cli,
    addr: &str,
    cwd: &Path,
    opts: TuiOptions,
    save_diagrams: impl FnMut(bool) -> Result<(), TuiError> + Send + 'static,
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
    run_blocking(
        remote::RemoteBackend(host),
        opts,
        model_source,
        save_diagrams,
    )
    .await
}
/// Starts the interactive client after the process edge has resolved all inputs.
pub(crate) async fn interactive(cli: &cli::Cli, startup: Startup, product: Product) -> ExitCode {
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
        binary,
        helper,
    } = startup;
    let no_color = edge::resolve_color(cli.color, &vars, snapshot.stdout_tty) == ColorArg::Never;
    if vars.contains_key(OsStr::new("DAL_DEBUG")) {
        eprintln!("[t1] edge {}ms", t0.elapsed().as_millis());
    }
    let opts = build_opts(cli, workspace, &config, &vars, &snapshot, no_color, binary);
    let save_diagrams = diagram_saver(config.clone(), config_path.clone());
    if let Some(addr) = cli.connect.as_deref() {
        return connect_remote(cli, addr, &cwd, opts, save_diagrams).await;
    }
    let dal_debug = vars.contains_key(OsStr::new("DAL_DEBUG"));
    if dal_debug {
        eprintln!("[t1] pre-host {}ms", t0.elapsed().as_millis());
    }
    // The local process edge owns the user configuration path, so the
    // host it starts may persist `SessionAndDefault` outcomes there. Remote
    // hosts never write: `connect_remote` above takes no host write path.
    let host = match Host::start_with_defaults(
        product,
        config,
        Env {
            vars,
            cwd,
            sandbox_helper: helper,
        },
        DefaultAuthority::Local { path: config_path },
    )
    .await
    {
        Ok(host) => host,
        Err(error) => return host_exit(&error, &data_root),
    };
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
    let code = run_blocking(host, opts, model_source, save_diagrams).await;
    let _ = shutdown.shutdown(std::time::Duration::from_secs(3)).await;
    code
}

async fn run_blocking<H, M, S>(
    host: H,
    opts: TuiOptions,
    model_source: M,
    save_diagrams: S,
) -> ExitCode
where
    H: dal_tui::backend::TuiHost,
    M: FnMut() -> Result<Vec<dal_tui::picker::ModelOption>, TuiError> + Send + 'static,
    S: FnMut(bool) -> Result<(), TuiError> + Send + 'static,
{
    let signals = match signal::Signals::start() {
        Ok(signals) => signals,
        Err(error) => {
            return tui_error(TuiError::Terminal(format!(
                "dalgon: cannot watch terminal signals: {error}\nRestart dalgon in a terminal."
            )));
        }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the process edge owns standard input"
    )]
    let io = signals.terminal(std::io::stdin());
    let result = tokio::task::spawn_blocking(move || {
        dal_tui::run_backend_with_settings_save(&host, &opts, &io, model_source, save_diagrams)
    })
    .await;
    let signal = signals.exit_status();
    drop(signals);
    if let Some(code) = signal {
        return ExitCode::from(code);
    }
    match result {
        Ok(Ok(_)) => exit::code(exit::ExitKind::Success),
        Ok(Err(error)) => tui_error(error),
        Err(error) => tui_error(TuiError::Terminal(format!(
            "dalgon: interactive terminal stopped: {error}\nRestart dalgon in a terminal."
        ))),
    }
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
            // A remote host never writes its own defaults (`serve` and wire
            // hosts refuse the save), so a remote selection is session-only:
            // `SessionAndDefault` would change the model and still report an
            // error.
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

        /// A real host whose catalog comes from loopback providers, listed by a
        /// real `RemoteHost` over a real local socket.
        #[cfg(unix)]
        mod served {
            use std::collections::{BTreeMap, BTreeSet};
            use std::ffi::OsString;
            use std::net::SocketAddr;
            use std::path::{Path, PathBuf};
            use std::sync::Arc;
            use std::time::Duration;

            use dal_agent::ext::{
                BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
                ModelRecord,
            };
            use dal_agent::{DefaultAuthority, Env, Host, Product, SessionRef};
            use dal_core::command::Save;
            use dal_core::{
                Caps, Command, Config, ConfigProduct, Expect, Family, ModelId, ModelRequest,
                ModelRoute, PageReq, Part, ServiceSet, ThinkingLevel, UpdateKind, Workspace,
            };
            use dal_tui::picker::ModelOption;
            use dal_wire::{RemoteDelivery, RemoteEndpoint, RemoteHost};
            use sonic_rs::{JsonValueTrait, Value};
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            use tokio::net::{TcpListener, TcpStream};
            use tokio::task::JoinSet;

            use super::super::super::remote_model_options;

            /// The model list each loopback provider serves.
            fn listing(path: &str) -> Option<&'static str> {
                match path {
                    "/openai/v1/models" => {
                        Some(r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#)
                    }
                    "/openrouter/api/v1/models" => Some(
                        r#"{"data":[{"id":"gpt-4o"},{"id":"openai/gpt-4o"},{"id":"free"},{"id":"openrouter/free"},{"id":"anthropic/claude-3"}]}"#,
                    ),
                    "/acme/v1/models" => Some(r#"{"data":[{"id":"gpt-4o"}]}"#),
                    _ => None,
                }
            }

            /// A cleartext model-list server on `127.0.0.1`.
            struct Loopback {
                address: SocketAddr,
                _server: JoinSet<()>,
            }

            impl Loopback {
                async fn start() -> Self {
                    let listener = TcpListener::bind(("127.0.0.1", 0))
                        .await
                        .expect("loopback listener");
                    let address = listener.local_addr().expect("loopback address");
                    let mut server = JoinSet::new();
                    server.spawn(accept(listener));
                    Self {
                        address,
                        _server: server,
                    }
                }
            }

            async fn accept(listener: TcpListener) {
                let mut connections = JoinSet::new();
                while let Ok((socket, _)) = listener.accept().await {
                    connections.spawn(answer(socket));
                }
            }

            async fn answer(socket: TcpStream) {
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let target = line.split_whitespace().nth(1).unwrap_or_default();
                let path = target.split('?').next().unwrap_or_default().to_owned();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let (status, body) = match listing(&path) {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", ""),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
                let _ = reader.get_mut().shutdown().await;
            }

            /// Fails a turn without touching the network; one test drives a
            /// prompt so the journal exists before its records are asserted.
            struct NeverInfer;

            impl ModelHandler for NeverInfer {
                fn run<'a>(
                    &'a self,
                    _request: ModelRequest,
                    _cx: ModelCx<'a>,
                ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
                    Box::pin(async { Err(ModelError::PrivateRounds) })
                }
            }

            fn synthetic(id: &str) -> ModelRecord {
                ModelRecord {
                    id: ModelId::parse(id).expect("synthetic model id"),
                    caps: Caps {
                        context_window: None,
                        thinking: Box::new([ThinkingLevel::Off]),
                        tool_use: false,
                        image_input: false,
                        custom_grammar: false,
                    },
                    handler: Arc::new(NeverInfer),
                    export: None,
                }
            }

            /// One started host that may save defaults into `config`.
            struct Fixture {
                _dir: tempfile::TempDir,
                _loopback: Loopback,
                host: Host,
                config: PathBuf,
                data: PathBuf,
                workspace: PathBuf,
                sockets: PathBuf,
            }

            impl Fixture {
                async fn start() -> Self {
                    Self::start_with(true).await
                }

                /// Starts a host the way `dalgon serve` does: it refuses every
                /// default write.
                async fn start_gated() -> Self {
                    Self::start_with(false).await
                }

                async fn start_with(may_save: bool) -> Self {
                    let loopback = Loopback::start().await;
                    let dir = tempfile::tempdir().expect("temporary root");
                    let data = dir.path().join("data");
                    let workspace = dir.path().join("w");
                    let config = dir.path().join("config").join("dal.toml");
                    let sockets = dir.path().join("sock");
                    for directory in [
                        data.as_path(),
                        workspace.as_path(),
                        config.parent().expect("config parent"),
                    ] {
                        std::fs::create_dir_all(directory).expect("fixture directory");
                    }
                    {
                        use std::os::unix::fs::DirBuilderExt as _;
                        std::fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&sockets)
                            .expect("private socket directory");
                    }
                    let address = loopback.address;
                    let user = format!(
                        r#"model = "openai/gpt-4o"

[retry]
request_max_retries = 0
stream_max_retries = 0

[providers.openai]
base_url = "http://{address}/openai/v1"

[providers.openrouter]
base_url = "http://{address}/openrouter/api/v1"
key_env = "OPENAI_API_KEY"

[providers.acme-proxy]
api = "openai_chat"
base_url = "http://{address}/acme/v1"
key_env = "OPENAI_API_KEY"
"#
                    );
                    std::fs::write(&config, &user).expect("user config");
                    let loaded = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))
                        .expect("test config loads");
                    let models = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::EMPTY)
                        .expect("extension name")
                        .model(synthetic("acme/fast"))
                        .model(synthetic("openrouter/free"))
                        .build()
                        .expect("fixture extension builds");
                    let product = Product {
                        name: "dal",
                        data_root: data.clone(),
                        defaults: "",
                        extensions: vec![models],
                        bundled: Vec::new(),
                    };
                    let mut vars = BTreeMap::new();
                    vars.insert(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"));
                    vars.insert(OsString::from("HOME"), dir.path().join("home").into());
                    vars.insert(
                        OsString::from("XDG_CACHE_HOME"),
                        dir.path().join("cache").into(),
                    );
                    let env = Env {
                        vars,
                        cwd: workspace.clone(),
                        sandbox_helper: None,
                    };
                    let authority = if may_save {
                        DefaultAuthority::Local {
                            path: config.clone(),
                        }
                    } else {
                        DefaultAuthority::Disabled
                    };
                    let host = Host::start_with_defaults(product, loaded, env, authority)
                        .await
                        .expect("host starts");
                    Self {
                        _dir: dir,
                        _loopback: loopback,
                        host,
                        config,
                        data,
                        workspace,
                        sockets,
                    }
                }

                /// Serves the host on a private local socket and runs `client`
                /// against a connected [`RemoteHost`] until it finishes.
                async fn served<F>(&self, client: impl FnOnce(RemoteHost) -> F)
                where
                    F: Future<Output = ()>,
                {
                    let socket = self.sockets.join("rpc.sock");
                    let serve_host = self.host.clone();
                    let listening = dal_wire::serve_local(&socket, None, None, move |transport| {
                        let host = serve_host.clone();
                        Box::pin(dal_wire::serve_rpc(host, transport))
                    });
                    let connected = async {
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                        while !socket.exists() {
                            assert!(tokio::time::Instant::now() < deadline, "socket appears");
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        let host = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket.clone()))
                            .await
                            .expect("connect");
                        client(host).await;
                    };
                    tokio::select! {
                        outcome = listening => panic!("the local server ended: {outcome:?}"),
                        outcome = tokio::time::timeout(Duration::from_secs(120), connected) => {
                            outcome.expect("the remote client finishes in time");
                        }
                    }
                }
            }

            fn api(family: Family, model: &str, provider: &str) -> ModelRoute {
                ModelRoute::Api {
                    family,
                    model: model.into(),
                    provider: Some(provider.into()),
                }
            }

            fn synthetic_route(id: &str) -> ModelRoute {
                ModelRoute::Synthetic { id: id.into() }
            }

            fn option_for<'a>(options: &'a [ModelOption], route: &ModelRoute) -> &'a ModelOption {
                options
                    .iter()
                    .find(|option| match &option.command {
                        Command::SetModel { model, .. } => model == route,
                        _ => false,
                    })
                    .unwrap_or_else(|| panic!("no picker option selects {route:?}"))
            }

            fn find_dir(root: &Path, name: &str) -> Option<PathBuf> {
                let mut stack = vec![root.to_path_buf()];
                while let Some(dir) = stack.pop() {
                    let Ok(entries) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        if entry.file_name().to_string_lossy() == name {
                            return Some(path);
                        }
                        stack.push(path);
                    }
                }
                None
            }

            /// One `model` journal record as `(API family or "synthetic", native
            /// or synthetic id, provider)`.
            type Journaled = (String, String, Option<String>);

            fn journal_models(data: &Path, session: &str) -> Vec<Journaled> {
                // The journal is created with the first record; until then
                // there is nothing to read.
                let Some(dir) = find_dir(data, session) else {
                    return Vec::new();
                };
                let text = std::fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
                text.lines()
                    .filter_map(|line| sonic_rs::from_str::<Value>(line).ok())
                    .filter(|record| record["type"].as_str() == Some("model"))
                    .map(|record| match record["route"]["synthetic"]["id"].as_str() {
                        Some(id) => ("synthetic".to_owned(), id.to_owned(), None),
                        None => (
                            record["api"].as_str().expect("api family").to_owned(),
                            record["model"].as_str().expect("native id").to_owned(),
                            record["provider"].as_str().map(str::to_owned),
                        ),
                    })
                    .collect()
            }

            fn journaled(family: &str, model: &str, provider: Option<&str>) -> Journaled {
                (
                    family.to_owned(),
                    model.to_owned(),
                    provider.map(str::to_owned),
                )
            }

            async fn assert_options_select_exact_routes(host: &RemoteHost) -> Vec<ModelOption> {
                let models = host.models().await.expect("the model list decodes");
                let options = remote_model_options(models.clone());
                assert_eq!(options.len(), 10, "one option per listed model");
                // The label is the row's name and its reference; every
                // label of this list is distinct, so none takes a kind
                // suffix.
                for (model, option) in models.iter().zip(&options) {
                    let reference = model.reference.as_deref().expect("a named row");
                    assert_eq!(option.label, format!("{} · {reference}", model.name));
                }
                let labels: BTreeSet<&str> =
                    options.iter().map(|option| option.label.as_str()).collect();
                assert_eq!(labels.len(), options.len(), "{labels:?}");
                // Each option selects its typed route, provider
                // included, and scopes API rows by `provider/native`.
                let wanted = [
                    (
                        api(Family::Responses, "gpt-4o", "openai"),
                        "openai/gpt-4o",
                        Some("openai/gpt-4o"),
                    ),
                    (
                        api(Family::Responses, "gpt-4o", "openrouter"),
                        "openrouter/gpt-4o",
                        Some("openrouter/gpt-4o"),
                    ),
                    (
                        api(Family::Chat, "gpt-4o", "acme-proxy"),
                        "acme-proxy/gpt-4o",
                        Some("acme-proxy/gpt-4o"),
                    ),
                    (
                        api(Family::Responses, "openrouter/free", "openrouter"),
                        "openrouter/openrouter/free",
                        Some("openrouter/openrouter/free"),
                    ),
                    (
                        api(Family::Responses, "free", "openrouter"),
                        "openrouter/free",
                        Some("openrouter/free"),
                    ),
                    (synthetic_route("acme/fast"), "acme/fast", None),
                    (synthetic_route("openrouter/free"), "openrouter/free", None),
                ];
                for (route, reference, scoped) in wanted {
                    let option = option_for(&options, &route);
                    assert!(
                        option.label.ends_with(&format!(" · {reference}")),
                        "{} names {reference}",
                        option.label
                    );
                    assert_eq!(option.scoped_id.as_deref(), scoped, "{route:?}");
                    assert_eq!(
                        option.command,
                        Command::SetModel {
                            model: route.clone(),
                            save: Save::SessionOnly,
                        }
                    );
                }
                options
            }

            #[tokio::test]
            async fn every_remote_selection_succeeds_on_a_serve_host_that_refuses_defaults() {
                // A `dalgon serve` host never writes defaults. A picker option
                // that asked to save one would change the model and still
                // fail, so each option must apply and report success.
                let fixture = Fixture::start_gated().await;
                let workspace = Workspace::new(fixture.workspace.clone()).expect("workspace");
                fixture
                    .served(|host| async move {
                        let models = host.models().await.expect("the model list decodes");
                        let options = remote_model_options(models);
                        let agent = host
                            .open(SessionRef::New {
                                workspace,
                                name: None,
                            })
                            .await
                            .expect("open a session");
                        let picks = [
                            api(Family::Responses, "gpt-4o", "openrouter"),
                            api(Family::Chat, "gpt-4o", "acme-proxy"),
                            synthetic_route("acme/fast"),
                        ];
                        for route in &picks {
                            let option = option_for(&options, route);
                            agent
                                .submit(option.command.clone())
                                .await
                                .unwrap_or_else(|error| {
                                    panic!("selecting {route:?} on a serve host: {error}")
                                });
                            let view = agent.view(PageReq::default()).await.expect("view");
                            assert_eq!(view.settings.model.as_ref(), Some(route));
                        }
                    })
                    .await;
            }

            #[tokio::test]
            async fn first_settings_update_reaches_a_fresh_remote_subscriber_exactly_once() {
                let fixture = Fixture::start_gated().await;
                let workspace = Workspace::new(fixture.workspace.clone()).expect("workspace");
                fixture
                    .served(|host| async move {
                        let models = host.models().await.expect("the model list decodes");
                        let options = remote_model_options(models);
                        let agent = host
                            .open(SessionRef::New {
                                workspace,
                                name: None,
                            })
                            .await
                            .expect("open a session");
                        let view = agent.view(PageReq::default()).await.expect("view");
                        let mut subscription = agent
                            .subscribe(Some((view.r#gen, view.seq)))
                            .await
                            .expect("subscribe");
                        let first = api(Family::Chat, "gpt-4o", "acme-proxy");
                        let second = api(Family::Responses, "gpt-4o", "openai");
                        for route in [&first, &second] {
                            let option = option_for(&options, route);
                            agent
                                .submit(option.command.clone())
                                .await
                                .unwrap_or_else(|error| panic!("selecting {route:?}: {error}"));
                        }
                        let barrier = agent.view(PageReq::default()).await.expect("view");
                        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                        let mut settings = Vec::new();
                        loop {
                            let delivery = tokio::time::timeout_at(deadline, subscription.next())
                                .await
                                .expect("the subscription reaches the observed head")
                                .expect("subscribe");
                            let RemoteDelivery::Update(update) = delivery else {
                                panic!("a fresh live subscription does not resync");
                            };
                            if let UpdateKind::Settings(seen) = &update.kind {
                                settings.push(seen.model.clone());
                            }
                            if (update.r#gen, update.seq) == (barrier.r#gen, barrier.seq) {
                                break;
                            }
                        }
                        assert_eq!(
                            settings.as_slice(),
                            [Some(first.clone()), Some(second.clone())],
                            "both selections arrive as settings, in order"
                        );
                    })
                    .await;
            }

            #[tokio::test]
            async fn every_option_of_a_real_list_selects_its_exact_route_and_provider() {
                let fixture = Fixture::start().await;
                let workspace = Workspace::new(fixture.workspace.clone()).expect("workspace");
                let data = fixture.data.clone();
                let config = fixture.config.clone();
                let initial = std::fs::read_to_string(&config).expect("initial config");
                fixture
                    .served(|host| async move {
                        let options = assert_options_select_exact_routes(&host).await;
                        // Submitting the options of both same-family rows, and
                        // returning to the first, sets the session model to
                        // each exact route: nothing rebinds to a twin.
                        let agent = host
                            .open(SessionRef::New {
                                workspace,
                                name: None,
                            })
                            .await
                            .expect("open a session");
                        let picks = [
                            api(Family::Chat, "gpt-4o", "acme-proxy"),
                            api(Family::Responses, "gpt-4o", "openai"),
                            api(Family::Responses, "gpt-4o", "openrouter"),
                            api(Family::Responses, "openrouter/free", "openrouter"),
                            synthetic_route("acme/fast"),
                            api(Family::Responses, "gpt-4o", "openai"),
                        ];
                        for route in &picks {
                            let option = option_for(&options, route);
                            agent
                                .submit(option.command.clone())
                                .await
                                .unwrap_or_else(|error| panic!("selecting {route:?}: {error}"));
                            let view = agent.view(PageReq::default()).await.expect("view");
                            assert_eq!(view.settings.model.as_ref(), Some(route));
                        }
                        // The server journaled each selection with its provider.
                        let session = agent.session().to_string();
                        // Selections alone stay lazy: one prompt creates the
                        // journal (its model fails closed above).
                        agent
                            .submit(Command::Prompt {
                                expect: Expect::Idle,
                                content: vec![Part::Text {
                                    text: "create the journal".into(),
                                }],
                            })
                            .await
                            .unwrap_or_else(|error| panic!("the journal-creating prompt: {error}"));
                        let wanted = [
                            journaled("openai_chat", "gpt-4o", Some("acme-proxy")),
                            journaled("openai_responses", "gpt-4o", Some("openai")),
                            journaled("openai_responses", "gpt-4o", Some("openrouter")),
                            journaled("openai_responses", "openrouter/free", Some("openrouter")),
                            journaled("synthetic", "acme/fast", None),
                            journaled("openai_responses", "gpt-4o", Some("openai")),
                        ];
                        let mut records = journal_models(&data, &session);
                        for _ in 0..50 {
                            if records.len() >= wanted.len() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            records = journal_models(&data, &session);
                        }
                        assert!(
                            records.ends_with(&wanted),
                            "journal model records {records:?} do not end with {wanted:?}"
                        );
                        // A remote selection is session-only: the server's
                        // saved default is untouched even on a host that may
                        // write one.
                        assert_eq!(
                            std::fs::read_to_string(&config).expect("saved config"),
                            initial,
                            "a remote picker selection never rewrites the server file"
                        );
                    })
                    .await;
            }
        }
    }
}
