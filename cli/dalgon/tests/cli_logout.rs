//! Binary-boundary tests for provider logout behavior.

#[expect(
    dead_code,
    reason = "fixture helpers are shared across binary test crates"
)]
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
    assert_eq!(output.stderr, [] as [u8; 0]);
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
    assert_eq!(output.stderr, [] as [u8; 0]);
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
    assert_eq!(output.stderr, [] as [u8; 0]);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-logout-token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-logout-token"));
    assert!(!fixture.auth_file().exists());
    let repeated = fixture.output(&["logout", "anthropic"])?;
    assert_eq!(repeated.status.code(), Some(0));
    assert_eq!(repeated.stdout, b"No stored credentials.\n");
    assert_eq!(repeated.stderr, [] as [u8; 0]);
    assert!(!fixture.auth_file().exists());
    Ok(())
}

const KEPT_AUTH: &str = r#"{"anthropic":{"kind":"api_key","key":"secret-keep-token"}}"#;

/// A provider the CLI does not sign in to is a usage error before the file is
/// read. A near spelling of a real provider is unknown too, so a typo can never
/// remove a neighbour's credential, and the credential file stays untouched.
#[test]
fn logout_refuses_an_unknown_provider_without_touching_the_auth_file() -> Result<(), Box<dyn Error>>
{
    for provider in [
        "nobody",
        "Anthropic",
        "anthropic ",
        "anthropic-x",
        "anthropi",
        "",
        "../auth",
    ] {
        let fixture = CliFixture::new()?;
        fixture.write_auth(KEPT_AUTH)?;

        let output = fixture.output(&["logout", provider])?;

        assert_eq!(output.status.code(), Some(124), "{provider:?}");
        assert_eq!(output.stdout, [] as [u8; 0], "{provider:?}");
        let stderr = String::from_utf8(output.stderr)?;
        let mut lines = stderr.lines();
        assert_eq!(
            lines.next(),
            Some(format!("dalgon: unknown provider \"{provider}\"").as_str()),
            "{stderr}"
        );
        assert!(
            lines
                .next()
                .is_some_and(|hint| hint.starts_with("Use one of ") && hint.contains("anthropic")),
            "{stderr}"
        );
        assert_eq!(lines.next(), None, "{stderr}");
        assert_eq!(
            std::fs::read_to_string(fixture.auth_file())?,
            KEPT_AUTH,
            "{provider:?} touched the credential file"
        );
    }
    Ok(())
}

/// The credential file is the only copy of every stored credential. When it
/// cannot be read, `logout` reports that and leaves it byte for byte as found,
/// for one provider and for all of them, and no secret held in it reaches
/// either stream.
#[test]
fn logout_refuses_a_corrupt_auth_file_and_leaves_it_untouched() -> Result<(), Box<dyn Error>> {
    for body in [
        "{not json",
        "",
        "[]",
        "null",
        r#"{"anthropic":{"kind":"api_key","key":"secret-corrupt-token"}"#,
        r#"{"anthropic":"secret-corrupt-token"}"#,
        r#"{"anthropic":{"kind":"api_key","key":"secret-corrupt-token","extra":1}}"#,
    ] {
        for args in [&["logout", "anthropic"][..], &["logout"][..]] {
            let fixture = CliFixture::new()?;
            fixture.write_auth(body)?;

            let output = fixture.output(args)?;

            assert_eq!(output.status.code(), Some(1), "{args:?} over {body:?}");
            assert_eq!(output.stdout, [] as [u8; 0], "{args:?} over {body:?}");
            let stderr = String::from_utf8(output.stderr)?;
            let mut lines = stderr.lines();
            assert!(
                lines
                    .next()
                    .is_some_and(|first| first.starts_with("dalgon: auth.json is not valid JSON: ")),
                "{stderr}"
            );
            assert_eq!(
                lines.next(),
                Some("Run dalgon login PROVIDER to write the file again."),
                "{stderr}"
            );
            assert_eq!(lines.next(), None, "{stderr}");
            assert!(
                !stderr.contains("secret-corrupt-token"),
                "the diagnostic echoes a stored secret: {stderr}"
            );
            assert_eq!(
                std::fs::read(fixture.auth_file())?,
                body.as_bytes(),
                "{args:?} rewrote the corrupt file {body:?}"
            );
        }
    }
    Ok(())
}

/// An auth file that other users can read is not trusted, and `logout` does not
/// quietly repair it or remove anything from it: the user fixes the mode.
#[cfg(unix)]
#[test]
fn logout_refuses_an_auth_file_open_to_other_users_and_leaves_it_untouched()
-> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    for args in [&["logout", "anthropic"][..], &["logout"][..]] {
        let fixture = CliFixture::new()?;
        fixture.write_auth(KEPT_AUTH)?;
        let path = fixture.auth_file();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;

        let output = fixture.output(args)?;

        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(output.stdout, [] as [u8; 0], "{args:?}");
        assert_eq!(
            output.stderr,
            format!(
                "dalgon: auth.json has group or other permissions\nRun chmod 600 {}.\n",
                path.display()
            )
            .into_bytes(),
            "{args:?}"
        );
        assert_eq!(std::fs::read_to_string(&path)?, KEPT_AUTH, "{args:?}");
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o644,
            "{args:?} changed the file mode"
        );
    }
    Ok(())
}
