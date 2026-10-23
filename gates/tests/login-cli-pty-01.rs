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

use dal_agent::login_fake::{
    ACCOUNT_ID, FakeOAuth, TokenReply, USER_CODE, VERIFICATION_URI, follow_authorize_url,
};
use dal_provider::{AuthStore, Credential};
use dal_store::FileMode;
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
    let mut command = Command::new(dalgon_binary()?);
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
        .map_or(text.len(), |index| start + index);
    Ok(text[start..end].into())
}

fn auth_json(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/dal/auth.json")
}

const SEED_OPENAI_KEY: &str = "sk-smoke-seed-openai-0001";

/// Writes an unrelated pre-existing credential to `auth.json` with mode 0600.
fn seed_auth(home: &Path) -> io::Result<()> {
    let auth_path = auth_json(home);
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = format!(
        "{{\n  \"openai\": {{\n    \"kind\": \"api_key\",\n    \"key\": \"{SEED_OPENAI_KEY}\"\n  }}\n}}\n"
    );
    dal_store::write_atomic(&auth_path, body.as_bytes(), FileMode::Mode0600)
        .map_err(io::Error::other)
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

#[tokio::test]
async fn copilot_blank_enter_on_a_pty_uses_default_domain_and_stores_credential() -> TestResult {
    let home = TestDir::new()?;
    seed_auth(home.path())?;
    let fake = FakeOAuth::start(TokenReply::Issue).await?;
    let mut command = login_command(home.path(), "github-copilot", fake.base())?;
    let pty = PtyProcess::spawn(&mut command, 80, 24)?;
    let before = termios_text(&pty)?;
    let (status, output, after) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for(b"Open the sign-in URL, then", SETTLE)?;
        pty.write(b"\r")?;
        let status = pty.wait_for_exit(SETTLE)?;
        let output = String::from_utf8_lossy(pty.output()).into_owned();
        let after = termios_text(&pty)?;
        Ok::<_, io::Error>((status, output, after))
    })
    .await??;
    assert!(status.success(), "login exits 0, got {status}");
    assert_eq!(before, after, "raw mode is restored after the sign-in");
    assert!(
        !fake.requests_to("/login/device/code").is_empty(),
        "device code was requested"
    );
    assert!(
        !fake.requests_to("/login/oauth/access_token").is_empty(),
        "access token was polled"
    );
    assert!(
        output.contains(&format!(
            "Open {VERIFICATION_URI} and enter code {USER_CODE}."
        )),
        "the device code prompt prints: {output}"
    );
    assert!(
        output.contains("Signed in to GitHub Copilot."),
        "the success message prints: {output}"
    );
    let store = AuthStore::load(auth_json(home.path()))?;
    assert!(
        store.credential("github-copilot").is_some(),
        "the Copilot credential was stored"
    );
    assert!(
        matches!(
            store.credential("openai"),
            Some(Credential::ApiKey { key }) if key.expose() == SEED_OPENAI_KEY
        ),
        "seeded unrelated credential was preserved"
    );
    Ok(())
}

#[tokio::test]
async fn copilot_ctrl_c_during_device_poll_cancels_and_restores_the_terminal() -> TestResult {
    let home = TestDir::new()?;
    seed_auth(home.path())?;
    let fake = FakeOAuth::start(TokenReply::DevicePending).await?;
    let mut command = login_command(home.path(), "github-copilot", fake.base())?;
    let pty = PtyProcess::spawn_controlling(&mut command, 80, 24)?;
    let before = termios_text(&pty)?;
    let pty = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for(b"Open the sign-in URL, then", SETTLE)?;
        pty.write(b"github.com\r")?;
        pty.wait_for(b"Waiting for sign-in...", SETTLE)?;
        Ok::<_, io::Error>(pty)
    })
    .await??;
    let deadline = tokio::time::Instant::now() + SETTLE;
    while fake.requests_to("/login/oauth/access_token").is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for access token poll request".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, output, after) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for_foreground(Duration::from_secs(10))?;
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
    assert_eq!(before, after, "raw mode is restored after cancellation");
    assert!(
        output.contains("Waiting for sign-in..."),
        "the progress prints before cancellation: {output}"
    );
    assert!(
        !output.contains("Signed in to GitHub Copilot."),
        "no success message prints on cancellation: {output}"
    );
    let store = AuthStore::load(auth_json(home.path()))?;
    assert!(
        store.credential("github-copilot").is_none(),
        "no Copilot credential was stored"
    );
    assert!(
        matches!(
            store.credential("openai"),
            Some(Credential::ApiKey { key }) if key.expose() == SEED_OPENAI_KEY
        ),
        "seeded unrelated credential was preserved"
    );
    Ok(())
}

#[tokio::test]
async fn required_oauth_prompt_blank_then_valid_paste_completes_exchange() -> TestResult {
    let home = TestDir::new()?;
    seed_auth(home.path())?;
    let fake = FakeOAuth::start(TokenReply::Issue).await?;
    let mut command = login_command(home.path(), "anthropic", fake.base())?;
    let pty = PtyProcess::spawn(&mut command, 80, 24)?;
    let before = termios_text(&pty)?;
    let (status, output, after) = tokio::task::spawn_blocking(move || {
        let mut pty = pty;
        pty.wait_for(b"Open the sign-in URL, then", SETTLE)?;
        pty.write(b"\rvalid-pasted-code\r")?;
        let status = pty.wait_for_exit(SETTLE)?;
        let output = String::from_utf8_lossy(pty.output()).into_owned();
        let after = termios_text(&pty)?;
        Ok::<_, io::Error>((status, output, after))
    })
    .await??;
    assert!(status.success(), "login exits 0, got {status}");
    assert_eq!(before, after, "raw mode is restored after the sign-in");
    let requests = fake.requests_to("/claude/v1/oauth/token");
    assert_eq!(
        requests.len(),
        1,
        "exactly one token exchange request was made"
    );
    let request: sonic_rs::Value = sonic_rs::from_str(&requests[0].body)?;
    assert_eq!(request["code"], sonic_rs::json!("valid-pasted-code"));
    let store = AuthStore::load(auth_json(home.path()))?;
    assert!(
        store.credential("anthropic").is_some(),
        "the pasted sign-in stores the credential"
    );
    assert!(
        matches!(
            store.credential("openai"),
            Some(Credential::ApiKey { key }) if key.expose() == SEED_OPENAI_KEY
        ),
        "seeded unrelated credential was preserved"
    );
    assert!(
        output.contains("Signed in to Anthropic."),
        "the success message prints: {output}"
    );
    Ok(())
}
