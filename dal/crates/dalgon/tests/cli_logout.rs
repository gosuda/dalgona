//! Binary-boundary tests for provider logout behavior.

mod support;

use std::error::Error;

use support::CliFixture;

#[test]
fn logout_warns_when_saved_model_uses_the_removed_provider() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("model = 'fast'\n[aliases]\nfast = 'anthropic/claude-sonnet-5'\n")?;
    fixture.write_auth(r#"{"anthropic":{"kind":"api_key","key":"secret-logout-token"}}"#)?;

    let output = fixture.output(&["logout", "anthropic"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"Removed credentials for anthropic.\n");
    assert_eq!(
        output.stderr,
        b"The saved model \"claude-sonnet-5\" needs anthropic credentials. Run dal to pick another model.\n"
    );
    assert!(!fixture.auth_file().exists());
    Ok(())
}

#[test]
fn logout_keeps_other_auth_and_suppresses_unrelated_warning() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_config("model = 'anthropic/claude-sonnet-5'\n")?;
    fixture.write_auth(
        r#"{"anthropic":{"kind":"api_key","key":"secret-anthropic-token"},"openai":{"kind":"api_key","key":"secret-openai-token"}}"#,
    )?;

    let output = fixture.output(&["logout", "openai"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"Removed credentials for openai.\n");
    assert!(output.stderr.is_empty());
    let auth = std::fs::read_to_string(fixture.auth_file())?;
    assert!(auth.contains("secret-anthropic-token"));
    assert!(!auth.contains("secret-openai-token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(fixture.auth_file())?.permissions().mode() & 0o777,
            0o600
        );
    }
    Ok(())
}

#[test]
fn logout_without_provider_removes_every_credential() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_auth(
        r#"{"anthropic":{"kind":"api_key","key":"secret-anthropic-token"},"openai":{"kind":"api_key","key":"secret-openai-token"}}"#,
    )?;

    let output = fixture.output(&["logout"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"Removed all credentials.\n");
    assert!(output.stderr.is_empty());
    assert!(!fixture.auth_file().exists());
    Ok(())
}

#[test]
fn logout_removes_the_last_credential_and_auth_file() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    fixture.write_auth(r#"{"anthropic":{"kind":"api_key","key":"secret-logout-token"}}"#)?;

    let output = fixture.output(&["logout", "anthropic"])?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"Removed credentials for anthropic.\n");
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-logout-token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-logout-token"));
    assert!(!fixture.auth_file().exists());
    let repeated = fixture.output(&["logout", "anthropic"])?;
    assert_eq!(repeated.status.code(), Some(0));
    assert_eq!(repeated.stdout, b"No stored credentials.\n");
    assert!(repeated.stderr.is_empty());
    assert!(!fixture.auth_file().exists());
    Ok(())
}
