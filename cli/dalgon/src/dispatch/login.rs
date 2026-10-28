use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use dal_agent::login::LoginOutcome;
use dal_agent::{Env, Host, HostError, Product};
use dal_provider::{
    CredentialKind, LoginIo, LoginProgress, Method, ProviderDef, ProviderError, StoredCredential,
    find,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{Startup, VarsMap, cli, edge, exit, two_lines};

#[cfg(feature = "test-support")]
/// Overrides the pinned sign-in endpoints with a literal-loopback base for a
/// test that runs the CLI sign-in against a local server. Release builds
/// never read it; the base must stay a literal loopback origin.
const TEST_ENDPOINTS_ENV: &str = "DAL_LOGIN_TEST_ENDPOINTS";

/// The provider ids dal signs in to, in display order.
pub(super) fn provider_ids() -> impl Iterator<Item = &'static str> {
    dal_provider::login_providers()
        .into_iter()
        .map(|def| def.id)
}

pub(super) fn is_login_provider(provider: &str) -> bool {
    provider_ids().any(|id| id == provider)
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Runs login, its status subcommand, or the provider picker.
///
/// Every sign-in, sign-out, and status read runs through the shared [`Host`]
/// operations, so the CLI publishes the same `LoginFinished` lifecycle and
/// policy path as the terminal and RPC front ends. Only the terminal
/// input/progress loop below is CLI-owned.
pub(crate) async fn run(args: cli::LoginArgs, startup: Startup, product: Product) -> ExitCode {
    if args.command.is_some() {
        return status_command(startup, product).await;
    }
    if args.device_auth
        && !args
            .provider
            .as_deref()
            .and_then(find)
            .is_some_and(|def| def.offers(Method::Device))
    {
        let ids: Vec<&str> = device_providers().map(|def| def.id).collect();
        return two_lines(
            [
                crate::cli::texts::device_auth_provider(&ids),
                crate::cli::texts::device_auth_provider_hint(
                    ids.first().copied().unwrap_or_default(),
                ),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    let Some(provider) = args.provider.as_deref() else {
        if args.api_key {
            return choose_api_key_provider(startup, product).await;
        }
        return choose_interactive_provider(startup, product).await;
    };
    let Some(def) = find(provider).filter(|def| is_login_provider(def.id)) else {
        return unknown_provider(provider);
    };
    if args.api_key {
        return login_api_key_flag(startup, product, def).await;
    }
    if args.device_auth {
        return login_device(startup, product, def).await;
    }
    login_browser_or_prompted(startup, product, def).await
}

/// The shared host with the paths one auth command reports against.
pub(super) struct AuthHost {
    pub(super) host: Host,
    pub(super) data_root: PathBuf,
    pub(super) vars: VarsMap,
    pub(super) auth_path: PathBuf,
}

#[cfg(feature = "test-support")]
pub(super) fn test_login_endpoints(
    vars: &VarsMap,
) -> Result<Option<dal_provider::LoginEndpoints>, ProviderError> {
    let Some(base) = vars
        .get(std::ffi::OsStr::new(TEST_ENDPOINTS_ENV))
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy())
    else {
        return Ok(None);
    };
    dal_provider::LoginEndpoints::loopback(&base, &dal_provider::EnvSnapshot::from_vars(vars))
        .map(Some)
}

/// Starts the shared host for one auth command. Test-support builds also
/// apply the loopback endpoint override when the test seam names one;
/// release builds never read it.
pub(super) async fn start_auth_host(
    startup: Startup,
    product: Product,
) -> Result<AuthHost, ExitCode> {
    let data_root = startup.data_root.clone();
    let vars = startup.vars.clone();
    let auth_path = data_root.join("auth.json");
    #[cfg(feature = "test-support")]
    let test_endpoints = test_login_endpoints(&startup.vars)
        .map_err(|error| provider_error("login", &error, &auth_path))?;
    let host = super::start_host(
        product,
        startup.config,
        Env {
            vars: startup.vars,
            cwd: startup.cwd,
            sandbox_helper: startup.helper,
        },
        &startup.data_root,
        &startup.config_path,
    )
    .await?;
    #[cfg(feature = "test-support")]
    if let Some(endpoints) = test_endpoints {
        host.set_login_endpoints(endpoints)
            .map_err(|error| host_auth_error("login", &error, &auth_path))?;
    }
    Ok(AuthHost {
        host,
        data_root,
        vars,
        auth_path,
    })
}

/// Shuts the auth host down after its command finishes, inside the process
/// shutdown ladder. An auth command holds no busy extension status, so it
/// has no quiet wait to spend.
pub(super) async fn shutdown_auth_host(host: Host) {
    let _closed = exit::Ladder::begin()
        .close_host_after_quiet_wait(host)
        .await;
}

/// Maps a host auth failure to its exit code, keeping the provider error
/// text the command dispatch already reports.
pub(super) fn host_auth_error(module: &str, error: &HostError, path: &Path) -> ExitCode {
    match error {
        HostError::Provider(error) => provider_error(module, error, path),
        error => {
            let data_root = path.parent().unwrap_or(path);
            let log_path = crate::edge::data_root_log_path(data_root);
            two_lines(
                crate::cli::texts::internal_error_at(module, &error.to_string(), &log_path),
                exit::ExitKind::Internal,
            )
        }
    }
}

pub(super) fn unknown_provider(provider: &str) -> ExitCode {
    let ids: Vec<&str> = provider_ids().collect();
    two_lines(
        [
            format!("dalgon: unknown provider \"{provider}\""),
            crate::cli::texts::login_provider_hint(&ids),
        ],
        exit::ExitKind::Usage,
    )
}

/// The providers with a device sign-in.
fn device_providers() -> impl Iterator<Item = &'static ProviderDef> {
    dal_provider::login_providers()
        .into_iter()
        .filter(|def| def.offers(Method::Device))
}

/// `--api-key`: read a piped key, for a provider that takes one.
async fn login_api_key_flag(
    startup: Startup,
    product: Product,
    def: &'static ProviderDef,
) -> ExitCode {
    if !def.offers(Method::ApiKey) {
        return two_lines(
            [
                crate::cli::texts::CODEX_API_KEY_UNSUPPORTED.into(),
                crate::cli::texts::CODEX_API_KEY_UNSUPPORTED_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    login_api_key(startup, product, def).await
}

async fn login_device(startup: Startup, product: Product, def: &'static ProviderDef) -> ExitCode {
    login_oauth(startup, product, def, Method::Device).await
}

async fn login_browser_or_prompted(
    startup: Startup,
    product: Product,
    def: &'static ProviderDef,
) -> ExitCode {
    if def.offers(Method::Browser) {
        if !edge::terminal_snapshot().stdin_tty {
            return no_terminal();
        }
        return login_oauth(startup, product, def, Method::Browser).await;
    }
    if def.offers(Method::Device) {
        return login_device(startup, product, def).await;
    }
    login_prompted_key(startup, product, def).await
}

async fn choose_api_key_provider(startup: Startup, product: Product) -> ExitCode {
    match prompt_for_provider() {
        Ok(Some(def)) => login_api_key_flag(startup, product, def).await,
        Ok(None) => exit::code(exit::ExitKind::Success),
        Err(exit) => exit,
    }
}

async fn choose_interactive_provider(startup: Startup, product: Product) -> ExitCode {
    match prompt_for_provider() {
        Ok(Some(def)) => login_browser_or_prompted(startup, product, def).await,
        Ok(None) => exit::code(exit::ExitKind::Success),
        Err(exit) => exit,
    }
}

fn prompt_for_provider() -> Result<Option<&'static ProviderDef>, ExitCode> {
    use std::io::BufRead as _;
    if !edge::terminal_snapshot().stdin_tty {
        return Err(no_terminal());
    }
    let _ = writeln!(std::io::stderr().lock(), "Sign in to a provider:");
    for (index, provider) in provider_ids().enumerate() {
        let _ = writeln!(std::io::stderr().lock(), "  {}. {provider}", index + 1);
    }
    let _ = write!(
        std::io::stderr().lock(),
        "Choose a provider (1-{}, or empty to cancel): ",
        provider_ids().count()
    );
    let _ = std::io::stderr().lock().flush();
    let mut selection = String::new();
    #[expect(
        clippy::disallowed_methods,
        reason = "process edge: the login chooser owns standard input"
    )]
    let read = std::io::stdin().lock().read_line(&mut selection);
    if read.is_err() {
        return Err(no_terminal());
    }
    let selection = selection.trim();
    if selection.is_empty() || selection == "\u{1b}" {
        let _ = writeln!(std::io::stdout().lock(), "No provider selected.");
        return Ok(None);
    }
    let chosen = selection
        .parse::<usize>()
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| provider_ids().nth(index))
        .or_else(|| provider_ids().find(|id| *id == selection));
    let Some(def) = chosen.and_then(find) else {
        return Err(unknown_provider(selection));
    };
    Ok(Some(def))
}

async fn login_api_key(startup: Startup, product: Product, def: &'static ProviderDef) -> ExitCode {
    if edge::terminal_snapshot().stdin_tty {
        return no_terminal();
    }
    let key = match read_piped_key().await {
        Ok(Some(key)) => key,
        Ok(None) => {
            return two_lines(
                [
                    crate::cli::texts::LOGIN_EMPTY_KEY.into(),
                    crate::cli::texts::LOGIN_PIPE_KEY_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
        Err(error) => {
            return two_lines(
                [
                    format!("dalgon: cannot read standard input: {error}"),
                    crate::cli::texts::LOGIN_PIPE_KEY_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
    };
    store_api_key(startup, product, def, key).await
}

async fn login_prompted_key(
    startup: Startup,
    product: Product,
    def: &'static ProviderDef,
) -> ExitCode {
    if !edge::terminal_snapshot().stdin_tty {
        return no_terminal();
    }
    let key = match prompt_for_key(def.id).await {
        Ok(key) if !key.is_empty() => key,
        Ok(_) => {
            return two_lines(
                [
                    crate::cli::texts::LOGIN_EMPTY_KEY.into(),
                    crate::cli::texts::LOGIN_EMPTY_KEY_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
        Err(error) => {
            return two_lines(
                [
                    format!("dalgon: cannot read standard input: {error}"),
                    crate::cli::texts::LOGIN_EMPTY_KEY_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
    };
    store_api_key(startup, product, def, key).await
}

async fn store_api_key(
    startup: Startup,
    product: Product,
    def: &'static ProviderDef,
    key: String,
) -> ExitCode {
    let AuthHost {
        host, auth_path, ..
    } = match start_auth_host(startup, product).await {
        Ok(auth) => auth,
        Err(code) => return code,
    };
    let (sender, pasted) = oneshot::channel();
    let _ = sender.send(key);
    let (io, _events) = LoginIo::channel(Some(pasted), CancellationToken::new());
    let outcome = host.login(def.id, Method::ApiKey, io).await;
    shutdown_auth_host(host).await;
    match outcome {
        Ok(_) => {
            let message = crate::cli::texts::saved_api_key(def.name);
            let _ = writeln!(std::io::stdout().lock(), "{message}");
            exit::code(exit::ExitKind::Success)
        }
        Err(error) => host_auth_error("login", &error, &auth_path),
    }
}

async fn login_oauth(
    startup: Startup,
    product: Product,
    def: &'static ProviderDef,
    method: Method,
) -> ExitCode {
    let AuthHost {
        host, auth_path, ..
    } = match start_auth_host(startup, product).await {
        Ok(auth) => auth,
        Err(code) => return code,
    };
    let cancel = CancellationToken::new();
    let (paste, paste_receiver) = oneshot::channel();
    let (io, events) = LoginIo::channel(Some(paste_receiver), cancel.clone());
    let mut signals = edge::ShutdownSignals::register();
    let outcome = Box::pin(super::drive(
        &cancel,
        &mut signals,
        run_with_paste(&host, def.id, method, io, events, paste),
    ))
    .await;
    shutdown_auth_host(host).await;
    match outcome {
        Err(signal) => exit::code(exit::ExitKind::Signal(signal)),
        Ok(Ok(outcome)) => {
            let message = success_message(def, &outcome);
            let _ = writeln!(std::io::stdout().lock(), "{message}");
            exit::code(exit::ExitKind::Success)
        }
        Ok(Err(error)) => host_auth_error("login", &error, &auth_path),
    }
}

/// Runs the login through the shared host, prints its progress, and reads
/// one masked paste from the keyboard once the flow asks for it.
async fn run_with_paste(
    host: &Host,
    provider: &str,
    method: Method,
    io: LoginIo,
    mut events: tokio::sync::mpsc::Receiver<LoginProgress>,
    paste: tokio::sync::oneshot::Sender<String>,
) -> Result<LoginOutcome, HostError> {
    let run = host.login(provider, method, io);
    tokio::pin!(run);
    let mut paste = Some(paste);
    let mut raw_mode = None;
    let mut prompt_default = None;
    let mut reader = std::pin::pin!(read_masked_value());
    loop {
        let reading = raw_mode.is_some() && paste.is_some();
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                if matches!(event, LoginProgress::AskPaste { .. }) && raw_mode.is_none() {
                    raw_mode = Some(RawModeGuard::enable().map_err(|error| {
                        HostError::Provider(ProviderError::AuthWrite {
                            reason: error.to_string(),
                        })
                    })?);
                }
                login_progress(&event);
                if let LoginProgress::AskPaste { default, .. } = event {
                    prompt_default = default;
                }
            }
            result = &mut run => return result,
            line = &mut reader, if reading => {
                let Ok(line) = line else {
                    return Err(HostError::Provider(ProviderError::LoginCancelled));
                };
                let answer = if line.is_empty() {
                    prompt_default.take()
                } else {
                    Some(line)
                };
                let Some(answer) = answer else {
                    reader.set(read_masked_value());
                    continue;
                };
                drop(raw_mode.take());
                if let Some(current) = paste.take() {
                    let _ = current.send(answer);
                }
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "process edge: the login flow reads the piped API key from standard input"
)]
async fn read_piped_key() -> std::io::Result<Option<String>> {
    use tokio::io::AsyncReadExt as _;
    let mut key = String::new();
    tokio::io::stdin().read_to_string(&mut key).await?;
    let key = key.trim_end_matches(['\r', '\n']);
    // A tab is a legal header character; any other control character, a line
    // break included, makes a key that no request can carry, and saving it would
    // report success until the first request fails.
    if key
        .chars()
        .any(|character| character.is_control() && character != '\t')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the key holds a line break or control character",
        ));
    }
    Ok((!key.is_empty()).then(|| key.to_owned()))
}

async fn prompt_for_key(provider: &str) -> io::Result<String> {
    let _raw_mode = RawModeGuard::enable()?;
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "Enter API key for {provider}: ")?;
    stderr.flush()?;
    drop(stderr);
    read_masked_value().await
}

async fn read_masked_value() -> io::Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    let mut value = String::new();
    loop {
        if event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Paste(text) => {
                    for character in text.chars() {
                        value.push(character);
                        write!(std::io::stderr().lock(), "•")?;
                    }
                    return Ok(value);
                }
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        writeln!(std::io::stderr().lock())?;
                        return Ok(value);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "input cancelled",
                        ));
                    }
                    KeyCode::Esc => {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "input cancelled",
                        ));
                    }
                    KeyCode::Char(character) => {
                        value.push(character);
                        write!(std::io::stderr().lock(), "•")?;
                    }
                    KeyCode::Backspace if value.pop().is_some() => {
                        write!(std::io::stderr().lock(), "\u{8} \u{8}")?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn login_progress(progress: &LoginProgress) {
    let mut stderr = std::io::stderr().lock();
    match progress {
        LoginProgress::OpenUrl { url } => {
            let _ = writeln!(
                stderr,
                "Open this URL in a browser:\n{url}\nWaiting for sign-in..."
            );
        }
        LoginProgress::ShowCode { url, code } => {
            let _ = writeln!(
                stderr,
                "Open {url} and enter code {code}.\nWaiting for sign-in..."
            );
        }
        LoginProgress::AskPaste { hint, .. } => {
            let _ = writeln!(stderr, "Open the sign-in URL, then {hint}");
        }
        LoginProgress::Exchanging => {
            let _ = writeln!(stderr, "Exchanging the sign-in code...");
        }
    }
}

fn success_message(def: &ProviderDef, outcome: &LoginOutcome) -> String {
    match (&outcome.method, &outcome.account) {
        (Method::ApiKey, _) => crate::cli::texts::SAVED_PROVIDER.into(),
        (_, Some(account)) => format!("Signed in with ChatGPT. Account {account}."),
        _ => crate::cli::texts::signed_in(def.name),
    }
}

async fn status_command(startup: Startup, product: Product) -> ExitCode {
    let AuthHost {
        host,
        data_root,
        vars,
        ..
    } = match start_auth_host(startup, product).await {
        Ok(auth) => auth,
        Err(code) => return code,
    };
    let code = status(&host, &data_root, &vars).await;
    shutdown_auth_host(host).await;
    code
}

async fn status(host: &Host, data_root: &Path, vars: &crate::VarsMap) -> ExitCode {
    let path = data_root.join("auth.json");
    let stored = match host.stored_credentials().await {
        Ok(stored) => stored,
        Err(error) => return host_auth_error("login status", &error, &path),
    };
    let cancel = CancellationToken::new();
    let readiness = match host.provider_readiness(&cancel).await {
        Ok(readiness) => readiness,
        Err(error) => return host_auth_error("login status", &error, &path),
    };
    let mut pending: std::collections::BTreeMap<&str, bool> = readiness
        .iter()
        .filter(|row| row.family == dal_core::Family::Bedrock)
        .map(|row| (row.provider.as_ref(), row.ready))
        .collect();
    let mut ready = false;
    let mut first_missing = None;
    let mut bedrock_missing = false;
    let mut output = String::new();
    for def in dal_provider::login_providers() {
        let provider = def.id;
        if let Some(bedrock_ready) = pending.remove(provider) {
            if bedrock_ready {
                ready = true;
                let _ = std::fmt::Write::write_fmt(
                    &mut output,
                    format_args!("{provider:<14} {:<15} {}\n", "ready", "bedrock"),
                );
            } else {
                first_missing.get_or_insert(provider);
                bedrock_missing = true;
                let _ = std::fmt::Write::write_fmt(
                    &mut output,
                    format_args!("{provider:<14} not configured\n"),
                );
            }
            continue;
        }
        let row = stored.iter().find(|row| row.provider.as_ref() == provider);
        let environment_key = def.key.is_some_and(|key| {
            key.env.iter().any(|name| {
                vars.get(std::ffi::OsStr::new(name))
                    .is_some_and(|value| !value.is_empty())
            })
        });
        if let Some(row) = row {
            ready |= !row.expired;
            let state = if row.expired { "expired" } else { "ready" };
            let _ = std::fmt::Write::write_fmt(
                &mut output,
                format_args!("{provider:<14} {:<15} {}\n", state, stored_kind(row)),
            );
        } else if environment_key {
            ready = true;
            let _ = std::fmt::Write::write_fmt(
                &mut output,
                format_args!("{provider:<14} {:<15} api_key\n", "ready"),
            );
        } else {
            first_missing.get_or_insert(provider);
            let _ = std::fmt::Write::write_fmt(
                &mut output,
                format_args!("{provider:<14} not configured\n"),
            );
        }
    }
    for (id, bedrock_ready) in pending {
        if bedrock_ready {
            ready = true;
            let _ = std::fmt::Write::write_fmt(
                &mut output,
                format_args!("{id:<14} {:<15} {}\n", "ready", "bedrock"),
            );
        } else {
            bedrock_missing = true;
            let _ =
                std::fmt::Write::write_fmt(&mut output, format_args!("{id:<14} not configured\n"));
        }
    }
    let _ = write!(std::io::stdout().lock(), "{output}");
    if ready {
        return exit::code(exit::ExitKind::Success);
    }
    let provider = first_missing
        .or_else(|| provider_ids().next())
        .unwrap_or_default();
    let _ = writeln!(
        std::io::stderr().lock(),
        "Run dalgon login {provider} to sign in."
    );
    if bedrock_missing {
        let _ = writeln!(
            std::io::stderr().lock(),
            "Set up AWS credentials for Bedrock via environment, shared config, or SSO."
        );
    }
    exit::code(exit::ExitKind::RequestedFailure)
}

fn stored_kind(row: &StoredCredential) -> String {
    match row.kind {
        CredentialKind::ApiKey => "api_key".to_owned(),
        CredentialKind::OAuth => match row.expires_at {
            Some(seconds) => jiff::Timestamp::from_second(seconds).ok().map_or_else(
                || "oauth".to_owned(),
                |timestamp| {
                    let date = timestamp
                        .to_zoned(jiff::tz::TimeZone::UTC)
                        .strftime("%Y-%m-%d")
                        .to_string();
                    format!("oauth, expires {date}")
                },
            ),
            None => "oauth".to_owned(),
        },
    }
}

fn no_terminal() -> ExitCode {
    two_lines(
        [
            crate::cli::texts::LOGIN_NEEDS_TERMINAL.into(),
            crate::cli::texts::LOGIN_NEEDS_TERMINAL_HINT.into(),
        ],
        exit::ExitKind::RequestedFailure,
    )
}

pub(super) fn provider_error(module: &str, error: &ProviderError, path: &Path) -> ExitCode {
    two_lines(
        provider_error_lines(module, error, path),
        exit::ExitKind::RequestedFailure,
    )
}

fn provider_error_lines(module: &str, error: &ProviderError, path: &Path) -> [String; 2] {
    let (what, hint) = match &error {
        ProviderError::AuthFileInvalid { message, .. } => (
            format!("dalgon: auth.json is not valid JSON: {message}"),
            crate::cli::texts::AUTH_INVALID_HINT.to_owned(),
        ),
        ProviderError::AuthFilePerms { path } => (
            format!("dalgon: {error}"),
            format!("Run chmod 600 {}.", path.display()),
        ),
        ProviderError::AuthFileSymlink { path } => (
            format!("dalgon: {error}"),
            format!("Replace {} with a regular file.", path.display()),
        ),
        ProviderError::AuthWrite { .. } => (
            format!("dalgon: {error}"),
            format!("Check the permissions of {} and try again.", path.display()),
        ),
        ProviderError::CallbackBind { port, .. } => (
            format!("dalgon: {module} failed: {error}"),
            format!("Free TCP port {port} on 127.0.0.1, then run dalgon login PROVIDER again."),
        ),
        ProviderError::StateMismatch => (
            format!("dalgon: {module} failed: {error}"),
            "Run dalgon login PROVIDER again without opening a second sign-in.".to_owned(),
        ),
        ProviderError::LoginTimeout => (
            format!("dalgon: {module} failed: {error}"),
            "Run dalgon login PROVIDER again and approve the sign-in within 15 minutes.".to_owned(),
        ),
        ProviderError::LoginCancelled => (
            format!("dalgon: {module}: {error}"),
            "Run dalgon login PROVIDER again to restart the sign-in.".to_owned(),
        ),
        _ => (
            format!("dalgon: {module} failed: {error}"),
            error
                .fix()
                .unwrap_or_else(|| crate::cli::texts::LOGIN_RETRY_HINT.into()),
        ),
    };
    [what, hint]
}

#[cfg(test)]
mod tests {
    use super::provider_error_lines;
    use dal_provider::ProviderError;
    use std::path::Path;

    #[test]
    fn callback_bind_names_port_and_free_remedy() {
        let lines = provider_error_lines(
            "login",
            &ProviderError::CallbackBind {
                port: 8471,
                reason: "in use".into(),
            },
            Path::new("/tmp/x/auth.json"),
        );
        assert_eq!(
            lines[0],
            "dalgon: login failed: could not listen on 127.0.0.1:8471: in use"
        );
        assert_eq!(
            lines[1],
            "Free TCP port 8471 on 127.0.0.1, then run dalgon login PROVIDER again."
        );
    }

    #[test]
    fn state_mismatch_rules_out_second_sign_in() {
        let lines = provider_error_lines(
            "login",
            &ProviderError::StateMismatch,
            Path::new("/tmp/x/auth.json"),
        );
        assert_eq!(
            lines[0],
            "dalgon: login failed: sign-in failed: the OAuth state does not match."
        );
        assert_eq!(
            lines[1],
            "Run dalgon login PROVIDER again without opening a second sign-in."
        );
    }

    #[test]
    fn login_timeout_names_fifteen_minute_window() {
        let lines = provider_error_lines(
            "login",
            &ProviderError::LoginTimeout,
            Path::new("/tmp/x/auth.json"),
        );
        assert_eq!(
            lines[0],
            "dalgon: login failed: sign-in timed out after 15 minutes."
        );
        assert_eq!(
            lines[1],
            "Run dalgon login PROVIDER again and approve the sign-in within 15 minutes."
        );
    }

    #[test]
    fn login_cancelled_points_at_restart() {
        let lines = provider_error_lines(
            "login",
            &ProviderError::LoginCancelled,
            Path::new("/tmp/x/auth.json"),
        );
        assert_eq!(lines[0], "dalgon: login: sign-in cancelled.");
        assert_eq!(
            lines[1],
            "Run dalgon login PROVIDER again to restart the sign-in."
        );
    }
}
