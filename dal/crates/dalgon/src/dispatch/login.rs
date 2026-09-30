use std::{
    io::{self, Write},
    path::Path,
    process::ExitCode,
    time::Duration,
};

use dal_provider::{
    AuthStore, Credential, LoginFlow, LoginProgress, OAuthCredential, ProviderError,
};
use tokio_util::sync::CancellationToken;

use crate::{Startup, cli, edge, exit, two_lines};

pub(super) const LOGIN_PROVIDERS: [&str; 3] = ["anthropic", "openai", "openai-codex"];
#[derive(Clone, Copy)]
enum OAuthMethod {
    Browser,
    Device,
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
pub(crate) async fn run(args: cli::LoginArgs, startup: Startup) -> ExitCode {
    if args.command.is_some() {
        return status(&startup.data_root, &startup.vars);
    }
    if args.device_auth && args.provider.as_deref() != Some("openai-codex") {
        return two_lines(
            [
                crate::cli::texts::DEVICE_AUTH_PROVIDER.into(),
                crate::cli::texts::DEVICE_AUTH_PROVIDER_HINT.into(),
            ],
            exit::ExitKind::RequestedFailure,
        );
    }
    let Some(provider) = args.provider.as_deref() else {
        return choose_provider(startup, args.api_key).await;
    };
    if !LOGIN_PROVIDERS.contains(&provider) {
        return two_lines(
            [
                format!("dalgon: unknown provider \"{provider}\""),
                crate::cli::texts::LOGIN_PROVIDER_HINT.into(),
            ],
            exit::ExitKind::Usage,
        );
    }
    if args.api_key {
        if provider == "openai-codex" {
            return two_lines(
                [
                    crate::cli::texts::CODEX_API_KEY_UNSUPPORTED.into(),
                    crate::cli::texts::CODEX_API_KEY_UNSUPPORTED_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
        return login_api_key(startup, provider).await;
    }
    match provider {
        "openai" => login_openai_key(startup, provider).await,
        "anthropic" if edge::terminal_snapshot().stdin_tty => {
            login_oauth(startup, provider, OAuthMethod::Browser).await
        }
        "openai-codex" if args.device_auth => {
            login_oauth(startup, provider, OAuthMethod::Device).await
        }
        "openai-codex" if edge::terminal_snapshot().stdin_tty => {
            login_oauth(startup, provider, OAuthMethod::Browser).await
        }
        "openai-codex" => no_terminal(),
        "anthropic" => no_terminal(),
        _ => exit::code(exit::ExitKind::Usage),
    }
}

async fn choose_provider(startup: Startup, api_key: bool) -> ExitCode {
    use std::io::BufRead as _;
    if !edge::terminal_snapshot().stdin_tty {
        return no_terminal();
    }
    let _ = writeln!(std::io::stderr().lock(), "Sign in to a provider:");
    for (index, provider) in LOGIN_PROVIDERS.iter().enumerate() {
        let _ = writeln!(std::io::stderr().lock(), "  {}. {provider}", index + 1);
    }
    let _ = write!(
        std::io::stderr().lock(),
        "Choose a provider (1-3, or empty to cancel): "
    );
    let _ = std::io::stderr().lock().flush();
    let mut selection = String::new();
    #[expect(
        clippy::disallowed_methods,
        reason = "R4 edge: the login chooser owns standard input"
    )]
    let read = std::io::stdin().lock().read_line(&mut selection);
    if read.is_err() {
        return no_terminal();
    }
    let selection = selection.trim();
    let provider = match selection {
        "1" | "anthropic" => "anthropic",
        "2" | "openai" => "openai",
        "3" | "openai-codex" => "openai-codex",
        "" | "\u{1b}" => {
            let _ = writeln!(std::io::stdout().lock(), "No provider selected.");
            return exit::code(exit::ExitKind::Success);
        }
        _ => {
            return two_lines(
                [
                    format!("dalgon: unknown provider \"{selection}\""),
                    crate::cli::texts::LOGIN_PROVIDER_HINT.into(),
                ],
                exit::ExitKind::Usage,
            );
        }
    };
    if api_key {
        if provider == "openai-codex" {
            return two_lines(
                [
                    crate::cli::texts::CODEX_API_KEY_UNSUPPORTED.into(),
                    crate::cli::texts::CODEX_API_KEY_UNSUPPORTED_HINT.into(),
                ],
                exit::ExitKind::RequestedFailure,
            );
        }
        return login_api_key(startup, provider).await;
    }
    if provider == "openai" {
        login_openai_key(startup, provider).await
    } else {
        login_oauth(startup, provider, OAuthMethod::Browser).await
    }
}

async fn login_api_key(startup: Startup, provider: &str) -> ExitCode {
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
    store_api_key(startup, provider, key).await
}

async fn login_openai_key(startup: Startup, provider: &str) -> ExitCode {
    if !edge::terminal_snapshot().stdin_tty {
        return no_terminal();
    }
    let key = match prompt_for_key(provider).await {
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
    store_api_key(startup, provider, key).await
}

async fn store_api_key(startup: Startup, provider: &str, key: String) -> ExitCode {
    let path = startup.data_root.join("auth.json");
    match dal_provider::store_api_key(&path, provider, key).await {
        Ok(()) => {
            let message = match provider {
                "anthropic" => crate::cli::texts::SAVED_ANTHROPIC,
                "openai" => crate::cli::texts::SAVED_OPENAI,
                _ => crate::cli::texts::SAVED_PROVIDER,
            };
            let _ = writeln!(std::io::stdout().lock(), "{message}");
            exit::code(exit::ExitKind::Success)
        }
        Err(error) => provider_error("login", error, &path),
    }
}

async fn login_oauth(startup: Startup, provider: &str, method: OAuthMethod) -> ExitCode {
    let auth_path = startup.data_root.join("auth.json");
    let mut store = match AuthStore::load(&auth_path) {
        Ok(store) => store,
        Err(error) => return provider_error("login", error, &auth_path),
    };
    let user_agent = dal_provider::user_agent(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        "",
        std::env::consts::ARCH,
    );
    let client = dal_provider::build_client();
    let flow = LoginFlow::new(
        provider,
        &mut store,
        client,
        user_agent,
        startup.data_root.join("cache"),
    );
    let mut flow = match flow {
        Ok(flow) => match method {
            OAuthMethod::Browser => flow,
            OAuthMethod::Device => flow.with_device_auth(),
        },
        Err(error) => return provider_error("login", error, &auth_path),
    };
    let paste_sender = flow.take_paste_sender();
    let _raw_mode = if paste_sender.is_some() {
        match RawModeGuard::enable() {
            Ok(guard) => Some(guard),
            Err(error) => {
                return provider_error(
                    "login",
                    ProviderError::AuthWrite {
                        reason: error.to_string(),
                    },
                    &auth_path,
                );
            }
        }
    } else {
        None
    };
    let cancel = CancellationToken::new();
    let progress: &(dyn Fn(LoginProgress) + Send + Sync) = &login_progress;
    let outcome = super::drive(&cancel, async {
        match paste_sender {
            Some(sender) => run_with_paste(&mut flow, sender, &cancel, progress).await,
            None => flow.run(progress, &cancel).await,
        }
    })
    .await;
    match outcome {
        Err(signal) => exit::code(exit::ExitKind::Signal(signal)),
        Ok(Ok(credential)) => {
            let message = success_message(provider, &credential);
            let _ = writeln!(std::io::stdout().lock(), "{message}");
            exit::code(exit::ExitKind::Success)
        }
        Ok(Err(error)) => provider_error("login", error, &auth_path),
    }
}

async fn run_with_paste(
    flow: &mut LoginFlow<'_>,
    sender: tokio::sync::oneshot::Sender<String>,
    cancel: &CancellationToken,
    progress: &(dyn Fn(LoginProgress) + Send + Sync),
) -> Result<Credential, ProviderError> {
    let run = flow.run(progress, cancel);
    tokio::pin!(run);
    let mut sender = Some(sender);
    loop {
        tokio::select! {
            biased;
            result = &mut run => return result,
            line = read_masked_value(), if sender.is_some() => {
                match line {
                    Ok(line) if line.is_empty() => {}
                    Ok(line) => {
                        if let Some(current) = sender.take() {
                            let _ = current.send(line);
                        }
                    }
                    Err(_) => return Err(ProviderError::LoginCancelled),
                }
            }
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "R4 edge: the login flow reads the piped API key from standard input"
)]
async fn read_piped_key() -> std::io::Result<Option<String>> {
    use tokio::io::AsyncReadExt as _;
    let mut key = String::new();
    tokio::io::stdin().read_to_string(&mut key).await?;
    let key = key.trim_end_matches(['\r', '\n']).to_owned();
    Ok((!key.is_empty()).then_some(key))
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
                    KeyCode::Backspace => {
                        if value.pop().is_some() {
                            write!(std::io::stderr().lock(), "\u{8} \u{8}")?;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn login_progress(progress: LoginProgress) {
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
        LoginProgress::AskPaste { hint } => {
            let _ = writeln!(stderr, "Open the sign-in URL, then {hint}");
        }
        LoginProgress::Exchanging => {
            let _ = writeln!(stderr, "Exchanging the sign-in code...");
        }
    }
}

fn success_message(provider: &str, credential: &Credential) -> String {
    match (provider, credential) {
        (
            "openai-codex",
            Credential::OAuth(OAuthCredential {
                account_id: Some(account),
                ..
            }),
        ) => {
            format!("Signed in with ChatGPT. Account {account}.")
        }
        ("anthropic", Credential::OAuth(_)) => crate::cli::texts::SAVED_CLAUDE.into(),
        ("openai-codex", _) => crate::cli::texts::SAVED_CODEX.into(),
        _ => crate::cli::texts::SAVED_PROVIDER.into(),
    }
}

fn status(data_root: &Path, vars: &crate::VarsMap) -> ExitCode {
    let path = data_root.join("auth.json");
    let store = match AuthStore::load(&path) {
        Ok(store) => store,
        Err(error) => return provider_error("login status", error, &path),
    };
    let mut ready = false;
    let mut first_missing = None;
    let mut output = String::new();
    for provider in LOGIN_PROVIDERS {
        let kind = stored_kind(&store, provider);
        let environment_key = match provider {
            "anthropic" => Some("ANTHROPIC_API_KEY"),
            "openai" => Some("OPENAI_API_KEY"),
            _ => None,
        }
        .and_then(|name| vars.get(std::ffi::OsStr::new(name)))
        .is_some_and(|key| !key.is_empty());
        if let Some(kind) = kind {
            ready = true;
            output.push_str(&format!("{provider:<14}{:<15}{kind}\n", "ready"));
        } else if environment_key {
            ready = true;
            output.push_str(&format!("{provider:<14}{:<15}api_key\n", "ready"));
        } else {
            first_missing.get_or_insert(provider);
            output.push_str(&format!("{provider:<14}not configured\n"));
        }
    }
    let _ = write!(std::io::stdout().lock(), "{output}");
    if ready {
        return exit::code(exit::ExitKind::Success);
    }
    let provider = first_missing.unwrap_or(LOGIN_PROVIDERS[0]);
    let _ = writeln!(
        std::io::stderr().lock(),
        "Run dalgon login {provider} to sign in."
    );
    exit::code(exit::ExitKind::RequestedFailure)
}

fn stored_kind(store: &AuthStore, provider: &str) -> Option<String> {
    match store.credential(provider)? {
        Credential::ApiKey { .. } => Some("api_key".to_owned()),
        Credential::OAuth(credential) => Some(match credential.expires_at {
            Some(seconds) => jiff::Timestamp::from_second(seconds)
                .ok()
                .map(|timestamp| {
                    let date = timestamp
                        .to_zoned(jiff::tz::TimeZone::UTC)
                        .strftime("%Y-%m-%d")
                        .to_string();
                    format!("oauth, expires {date}")
                })
                .unwrap_or_else(|| "oauth".to_owned()),
            None => "oauth".to_owned(),
        }),
        Credential::None => None,
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

pub(super) fn provider_error(module: &str, error: ProviderError, path: &Path) -> ExitCode {
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
        _ => (
            format!("dalgon: {module} failed: {error}"),
            error
                .fix()
                .unwrap_or_else(|| crate::cli::texts::LOGIN_RETRY_HINT.into()),
        ),
    };
    two_lines([what, hint], exit::ExitKind::RequestedFailure)
}
