//! Interactive terminal dispatch over the local or remote host contract.

use std::ffi::OsStr;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dal_agent::{Env, Host, Product};
use dal_core::Screen as ConfigScreen;
use dal_tui::{EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};
use dal_wire::{RemoteEndpoint, RemoteHost};

use crate::cli::{self, ColorArg};
use crate::edge;
use crate::exit;
use crate::{Startup, host_exit, session_ref, two_lines};

mod remote;
mod signal;

/// Starts the interactive client after the process edge has resolved all inputs.
pub(crate) async fn interactive(cli: &cli::Cli, startup: Startup, product: Product) -> ExitCode {
    let snapshot = edge::terminal_snapshot();
    let term = captured(&startup.vars, "TERM");
    if !snapshot.stdin_tty || !snapshot.stdout_tty {
        return two_lines(
            [
                cli::texts::NO_PROMPT.into(),
                cli::texts::NO_PROMPT_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    if term.is_none_or(|value| value == "dumb") {
        return two_lines(
            cli::texts::term_not_addressable(term.unwrap_or("unset")),
            exit::ExitKind::RequestedFailure,
        );
    }
    let width = crossterm::terminal::size().map_or(80, |(columns, _)| usize::from(columns));
    if width < 40 {
        return two_lines(
            cli::texts::terminal_too_narrow(width),
            exit::ExitKind::RequestedFailure,
        );
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
    } = startup;
    let no_color = edge::resolve_color(cli.color, &vars, snapshot.stdout_tty) == ColorArg::Never;
    let env = env_facts(&vars, snapshot.stdin_tty);
    let opts = TuiOptions {
        session: session_ref(cli, workspace),
        screen: match config.screen() {
            ConfigScreen::Inline => Screen::Inline,
            ConfigScreen::Fullscreen => Screen::Fullscreen,
        },
        theme_request: match config.theme() {
            "auto" => ThemeRequest::Auto,
            "palette" => ThemeRequest::Palette,
            name => ThemeRequest::Named(name.into()),
        },
        images: config.images(),
        diagrams: config.tui().diagrams,
        motion: config.motion() && !env.no_motion,
        editor: captured(&vars, "VISUAL")
            .or_else(|| captured(&vars, "EDITOR"))
            .filter(|value| !value.is_empty())
            .unwrap_or(if cfg!(windows) { "notepad" } else { "vi" })
            .into(),
        color: dal_tui::term::color_mode(&env, no_color),
        env,
        rt: tokio::runtime::Handle::current(),
    };
    let mut saved_config = config.clone();
    let save_config_path = config_path.clone();
    let save_diagrams =
        move |enabled| save_diagrams_to(&save_config_path, &mut saved_config, enabled);
    if let Some(addr) = &cli.connect {
        return connect_remote(cli, &cwd, addr, opts, save_diagrams).await;
    }
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
    let shutdown = host.clone();
    let model_host = host.clone();
    let model_rt = opts.rt.clone();
    let model_source = move || -> Result<Vec<dal_tui::picker::ModelOption>, TuiError> {
        let models = model_rt.block_on(model_host.models(None))?;
        Ok(dal_tui::picker::model_options(models))
    };
    let code = run_blocking(host, opts, model_source, save_diagrams).await;
    let _ = shutdown.shutdown(std::time::Duration::from_secs(3)).await;
    code
}

/// Captures terminal-relevant environment facts from the process edge.
fn env_facts(vars: &crate::VarsMap, stdin_tty: bool) -> EnvFacts {
    EnvFacts {
        stdin_tty,
        path: vars.get(OsStr::new("PATH")).cloned(),
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
    }
}

/// Persists a diagrams on/off toggle into the user's `dal.toml`.
fn save_diagrams_to(
    config_path: &Path,
    config: &mut crate::Config,
    enabled: bool,
) -> Result<(), TuiError> {
    let user_toml = edge::read_user_config(config_path)
        .map_err(|error| TuiError::Terminal(format!("dalgon: cannot read dal.toml: {error}")))?;
    let updated = config
        .update_tui_diagrams(enabled, user_toml.as_deref())
        .map_err(|error| TuiError::Terminal(format!("dalgon: cannot update dal.toml: {error}")))?;
    let config_dir = config_path.parent().ok_or_else(|| {
        TuiError::Terminal("dalgon: config path has no parent directory".to_owned())
    })?;
    dal_store::create_private_dir_all(config_dir).map_err(|error| {
        TuiError::Terminal(format!("dalgon: cannot create config directory: {error}"))
    })?;
    dal_store::write_atomic(
        config_path,
        updated.as_bytes(),
        dal_store::FileMode::Mode0600,
    )
    .map_err(|error| {
        TuiError::Terminal(format!(
            "dalgon: cannot save {}: {error}",
            config_path.display()
        ))
    })?;
    Ok(())
}

/// Connects to a remote dal host and runs the interactive client over it.
async fn connect_remote(
    cli: &cli::Cli,
    cwd: &Path,
    addr: &str,
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
        reason = "R4 edge: the process edge owns standard input"
    )]
    let io = signals.terminal(std::io::stdin());
    let result = tokio::task::spawn_blocking(move || {
        dal_tui::run_backend_with_settings_save(host, opts, io, model_source, save_diagrams)
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

fn remote_model_options(
    models: Vec<dal_wire::remote::RemoteModel>,
) -> Vec<dal_tui::picker::ModelOption> {
    models
        .into_iter()
        .map(|model| {
            let reference = match model.provider.as_str() {
                "openai" | "openai-codex" | "anthropic" => {
                    format!("{}/{}", model.provider, model.id)
                }
                _ => model.id,
            };
            dal_tui::picker::ModelOption {
                label: format!("{} · {reference}", model.name),
                command: dal_core::Command::Run {
                    name: "model".into(),
                    args: reference.into(),
                    expected: None,
                },
            }
        })
        .collect()
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
}
