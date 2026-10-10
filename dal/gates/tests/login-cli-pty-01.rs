#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Binary-boundary PTY tests for the interactive CLI sign-in: the real
//! `dalgon login` binary runs on a real pseudoterminal against a loopback
//! OAuth server, proving the progress output, the stored credential, Ctrl-C
//! cancellation, and terminal restoration.

#[expect(
    dead_code,
    reason = "PTY support includes helpers used by other gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, io, path::Path, process::Command, time::Duration};

use dal_agent::login_fake::{ACCOUNT_ID, FakeOAuth, TokenReply, follow_authorize_url};
use pty::PtyProcess;
use support::{TestDir, dalgon_binary};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const SETTLE: Duration = Duration::from_secs(60);

/// Builds an isolated `dalgon login` command whose sign-in endpoints point
/// at the loopback OAuth server.
fn login_command(home: &Path, provider: &str, endpoints: &str) -> io::Result<Command> {
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share/dal");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    let mut command = Command::new(dalgon_binary("dalgon")?);
    command
        .arg("login")
        .arg(provider)
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("DAL_NO_MOTION", "1")
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("DAL_LOGIN_TEST_ENDPOINTS", endpoints);
    Ok(command)
}

/// Compares slave-side terminal attributes by their flag words.
fn termios_text(pty: &PtyProcess) -> io::Result<String> {
    let termios = pty.termios()?;
    Ok(format!(
        "{:?} {:?} {:?} {:?}",
        termios.input_modes, termios.output_modes, termios.control_modes, termios.local_modes
    ))
}

/// Extracts the loopback authorize URL from the captured terminal output.
fn authorize_url(output: &[u8]) -> io::Result<String> {
    let text = String::from_utf8_lossy(output);
    let start = text
        .find("http://127.0.0.1:")
        .ok_or_else(|| io::Error::other("no authorize URL in the terminal output"))?;
    let end = text[start..]
        .find(['\r', '\n', ' '])
        .map(|index| start + index)
        .unwrap_or(text.len());
    Ok(text[start..end].into())
}

fn auth_json(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/dal/auth.json")
}

#[tokio::test]
async fn browser_login_on_a_pty_stores_the_credential_and_restores_the_terminal() -> TestResult {
    let home = TestDir::new()?;
    let fake = FakeOAuth::start(TokenReply::Issue).await?;
    let mut command = login_command(home.path(), "openai-codex", fake.base())?;
    let pty = PtyProcess::spawn(&mut command, 80, 24)?;
    let before = termios_text(&pty)?;
    let (pty, url) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for(b"Open this URL", SETTLE)?;
        let url = authorize_url(pty.output())?;
        Ok::<_, io::Error>((pty, url))
    })
    .await??;
    follow_authorize_url(&url, "auth-code").await?;
    let (status, output, after) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        let status = pty.wait_for_exit(SETTLE)?;
        let output = String::from_utf8_lossy(pty.output()).into_owned();
        let after = termios_text(&pty)?;
        Ok::<_, io::Error>((status, output, after))
    })
    .await??;
    assert!(status.success(), "login exits 0, got {status}");
    assert!(
        auth_json(home.path()).is_file(),
        "the loopback callback stores the credential"
    );
    assert!(
        output.contains(&format!("Signed in with ChatGPT. Account {ACCOUNT_ID}.")),
        "the account-specific success message prints: {output}"
    );
    assert_eq!(before, after, "raw mode is restored after the sign-in");
    Ok(())
}

#[tokio::test]
async fn ctrl_c_on_a_pty_cancels_the_wait_and_restores_the_terminal() -> TestResult {
    let home = TestDir::new()?;
    let fake = FakeOAuth::start(TokenReply::Issue).await?;
    let mut command = login_command(home.path(), "anthropic", fake.base())?;
    let pty = PtyProcess::spawn(&mut command, 80, 24)?;
    let before = termios_text(&pty)?;
    let (status, output, after) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for(b"Open the sign-in URL, then", SETTLE)?;
        pty.write(b"\x03")?;
        let status = pty.wait_for_exit(SETTLE)?;
        let output = String::from_utf8_lossy(pty.output()).into_owned();
        let after = termios_text(&pty)?;
        Ok::<_, io::Error>((status, output, after))
    })
    .await??;
    assert!(
        !status.success(),
        "cancellation fails the login, got {status}"
    );
    assert!(
        !auth_json(home.path()).exists(),
        "cancellation stores no credential"
    );
    assert!(
        output.contains("Waiting for sign-in"),
        "the progress prints before cancellation: {output}"
    );
    assert_eq!(before, after, "raw mode is restored after cancellation");
    Ok(())
}
