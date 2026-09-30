//! Binary-boundary tests for provider login and status commands.

mod support;

use std::{error::Error, fs};

use support::CliFixture;

#[test]
fn piped_api_key_is_stored_privately_without_echo() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let output = fixture.output_with_stdin(
        &["login", "anthropic", "--api-key"],
        b"secret-login-token\n",
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"Saved Anthropic credentials. Run dal to pick a model.\n"
    );
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-login-token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-login-token"));
    let saved = fs::read_to_string(fixture.auth_file())?;
    assert!(saved.contains("secret-login-token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(fixture.auth_file())?.permissions().mode() & 0o777,
            0o600
        );
    }
    let status = fixture.output(&["login", "status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(
        status.stdout,
        b"anthropic     ready          api_key\nopenai        not configured\nopenai-codex  not configured\n"
    );
    assert!(status.stderr.is_empty());
    Ok(())
}

#[test]
fn piped_api_key_preserves_other_provider_credentials() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_auth(r#"{"openai":{"kind":"api_key","key":"secret-existing-openai"}}"#)?;

    let output = fixture.output_with_stdin(
        &["login", "anthropic", "--api-key"],
        b"secret-new-anthropic\n",
    )?;

    assert_eq!(output.status.code(), Some(0));
    let saved = fs::read_to_string(fixture.auth_file())?;
    assert!(saved.contains("secret-existing-openai"));
    assert!(saved.contains("secret-new-anthropic"));
    Ok(())
}

#[test]
fn headless_codex_login_uses_login_needs_terminal_error() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["login", "openai-codex"])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"dalgon: login needs a terminal: no prompt can be shown\nRun it interactively, or pipe the key: printf %s \"$KEY\" | dalgon login anthropic --api-key\n"
    );
    Ok(())
}

#[test]
fn status_reports_oauth_expiry_without_exposing_tokens() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_auth(
        r#"{"openai-codex":{"kind":"oauth","access_token":"access-secret","refresh_token":"refresh-secret","expires_at":1790380800,"id_token":"id-secret","account_id":"account-1"}}"#,
    )?;

    let output = fixture.output(&["login", "status"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        b"anthropic     not configured\nopenai        not configured\nopenai-codex  ready          oauth, expires 2026-09-26\n"
    );
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    Ok(())
}

#[test]
fn status_with_no_credentials_prints_the_first_login_hint() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let output = fixture.output(&["login", "status"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stdout,
        b"anthropic     not configured\nopenai        not configured\nopenai-codex  not configured\n"
    );
    assert_eq!(output.stderr, b"Run dalgon login anthropic to sign in.\n");
    Ok(())
}
