//! Binary-boundary tests for provider login and status commands.

mod support;

use dal_agent::login::login_providers;
use std::{error::Error, fs};

use support::CliFixture;

/// One parsed `login status` row: the provider id plus the state columns.
///
/// The renderer pads the id to width 14 (`{id:<14}{state:<15}{kind}`), so ids
/// longer than 14 run into the state column with no gap; the split searches
/// for the state marker from the right instead of splitting on whitespace.
fn status_row(line: &str) -> Result<(String, String), std::io::Error> {
    for marker in ["not configured", "ready", "expired"] {
        if let Some(pos) = line.rfind(marker) {
            let (id, rest) = line.split_at(pos);
            return Ok((id.trim_end().to_owned(), rest.to_owned()));
        }
    }
    Err(std::io::Error::other(format!(
        "unrecognised login status row: {line:?}"
    )))
}

/// Identity invariant over the printed catalog: exactly the login-capable
/// provider set, in id order, with no drops or extras. The expected ids come
/// from the same `login_providers()` source of truth the status renderer
/// reads, so the catalog can grow without re-pinning rows — while any cut,
/// addition, or reorder on either side fails loudly.
fn assert_full_catalog(rows: &[(String, String)]) {
    let ids: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
    // The printed order must equal the sorted source order; sorting the
    // expectation (rather than trusting the source pre-sorted) is what pins
    // the renderer's id-ascending contract.
    let mut expected: Vec<&str> = login_providers().iter().map(|def| def.id).collect();
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "status catalog is not the id-ordered login set"
    );
    for required in ["abliteration", "anthropic", "openai", "openai-codex"] {
        assert!(
            ids.contains(&required),
            "status catalog is missing {required}: {ids:?}"
        );
    }
    // `abliteration` sorts first; pinning the head proves the catalog is the
    // full id-ordered list rather than the old three-row vendor pin.
    assert_eq!(ids.first(), Some(&"abliteration"));
}

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
    assert_eq!(output.stderr, [] as [u8; 0]);
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
    let text = String::from_utf8_lossy(&status.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    assert_full_catalog(&rows);
    for (id, rest) in &rows {
        if id == "anthropic" {
            assert!(rest.split_whitespace().eq(["ready", "api_key"]));
        } else {
            assert_eq!(rest, "not configured", "{id} changed state unexpectedly");
        }
    }
    assert_eq!(status.stderr, [] as [u8; 0]);
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
    assert_eq!(output.stdout, [] as [u8; 0]);
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

    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    assert_full_catalog(&rows);
    for (id, rest) in &rows {
        if id == "openai-codex" {
            assert!(
                rest.split_whitespace()
                    .eq(["expired", "oauth,", "expires", "2026-09-26"])
            );
        } else {
            assert_eq!(rest, "not configured", "{id} changed state unexpectedly");
        }
    }
    // The first id in the catalog owns the sign-in hint.
    assert_eq!(
        output.stderr,
        b"Run dalgon login abliteration to sign in.\n"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
    Ok(())
}

#[test]
fn status_with_no_credentials_prints_the_first_login_hint() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let output = fixture.output(&["login", "status"])?;

    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    assert_full_catalog(&rows);
    for (id, rest) in &rows {
        assert_eq!(rest, "not configured", "{id} changed state unexpectedly");
    }
    // The first id in the catalog owns the sign-in hint.
    assert_eq!(
        output.stderr,
        b"Run dalgon login abliteration to sign in.\n"
    );
    Ok(())
}

/// Runs `login status` with the fixture isolation plus extra captured
/// environment. Mirrors `support::CliFixture` (`env_clear`, `PATH`,
/// `HOME`, `XDG_*`, isolated cwd) so AWS inputs stay captured, isolated,
/// and offline; `AWS_EC2_METADATA_DISABLED` is always set to keep the
/// ambient probe off IMDS/network. No live keys, network, or commands.
#[expect(
    clippy::disallowed_methods,
    reason = "test harness: the fixture launches the built dalgon binary and forwards its own environment"
)]
fn bedrock_status(
    fixture: &CliFixture,
    extra_env: &[(&str, &str)],
) -> Result<std::process::Output, std::io::Error> {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_dalgon"));
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .args(["login", "status"])
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .env("XDG_DATA_HOME", &fixture.data)
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_REGION", "us-east-1")
        .current_dir(fixture.cwd());
    for (name, value) in extra_env {
        command.env(name, value);
    }
    command.output()
}

/// Configures one ambient Bedrock route without any login-provider
/// credential. `auth = "none"` keeps the route credential-less at the
/// config layer so readiness comes only from the captured AWS chain or
/// the stored bearer; the status path performs no network.
fn write_bedrock_config(fixture: &CliFixture) -> std::io::Result<()> {
    fixture.write_config(
        "[providers.my-bedrock]\n\
         api = \"bedrock\"\n\
         base_url = \"http://127.0.0.1:9\"\n\
         auth = \"none\"\n",
    )
}

fn find_bedrock_row(rows: &[(String, String)]) -> Option<&(String, String)> {
    rows.iter().find(|(id, _)| id == "my-bedrock")
}

#[test]
fn configured_bedrock_with_static_env_keys_reports_ready() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    write_bedrock_config(&fixture)?;
    // Placeholder inputs only: non-empty values make the captured static
    // chain present without any real credential or network.
    let output = bedrock_status(
        &fixture,
        &[
            ("AWS_ACCESS_KEY_ID", "test-key-id"),
            ("AWS_SECRET_ACCESS_KEY", "test-secret-key"),
        ],
    )?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "configured Bedrock with captured static keys must succeed"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    let (_, rest) = find_bedrock_row(&rows).unwrap_or_else(|| {
        panic!("configured Bedrock provider missing from login status: {text:?}")
    });
    assert_eq!(
        rest.split_whitespace().next(),
        Some("ready"),
        "configured Bedrock with static keys must be ready: {rest:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("dalgon login my-bedrock"),
        "Bedrock has no interactive login: {stderr:?}"
    );
    Ok(())
}

#[test]
fn configured_bedrock_without_credentials_reports_not_configured_with_aws_hint()
-> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    write_bedrock_config(&fixture)?;
    let output = bedrock_status(&fixture, &[])?;

    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    let (_, rest) = find_bedrock_row(&rows).unwrap_or_else(|| {
        panic!("configured Bedrock provider missing from login status: {text:?}")
    });
    assert_eq!(
        rest, "not configured",
        "absent Bedrock credentials must read not configured"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("AWS"),
        "absent Bedrock must hint AWS environment/shared-config/SSO setup: {stderr:?}"
    );
    assert!(
        !stderr.contains("dalgon login my-bedrock"),
        "absent Bedrock must never suggest an interactive login: {stderr:?}"
    );
    Ok(())
}

#[test]
fn configured_bedrock_with_bearer_env_reports_ready() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    write_bedrock_config(&fixture)?;
    // Bearer-env proof only: placeholder token via the captured bearer
    // environment source, no static keys and no auth.json. A custom
    // `my-bedrock` auth.json member is not a valid store shape today
    // (AuthFileVisitor accepts only built-in PROVIDERS rows), so no
    // stored-only custom-bearer proof is baked here; see READY report.
    let output = bedrock_status(
        &fixture,
        &[("AWS_BEARER_TOKEN_BEDROCK", "test-bearer-placeholder")],
    )?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "Bedrock bearer env must succeed"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    let (_, rest) = find_bedrock_row(&rows).unwrap_or_else(|| {
        panic!("configured Bedrock provider missing from login status: {text:?}")
    });
    assert_eq!(
        rest.split_whitespace().next(),
        Some("ready"),
        "Bedrock bearer env must be ready: {rest:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("dalgon login my-bedrock"),
        "Bedrock has no interactive login: {stderr:?}"
    );
    Ok(())
}

#[test]
fn configured_groq_override_to_bedrock_with_stored_key_reports_single_ready_row()
-> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    // Stored-only acceptance on a valid store shape: `groq` is an existing
    // Compatible preset, so `[providers.groq] api = "bedrock"` decodes and
    // its `api_key` auth.json member loads. Control note: `groq` sits in
    // the login catalog, so this row is ready GREEN pre-join via the login
    // path; post-join the CLI must still show exactly one `groq` row (no
    // login + Bedrock double-count) with ready status.
    fixture
        .write_config("[providers.groq]\napi = \"bedrock\"\nbase_url = \"http://127.0.0.1:9\"\n")?;
    fixture.write_auth(r#"{"groq":{"kind":"api_key","key":"test-groq-placeholder"}}"#)?;
    let output = bedrock_status(&fixture, &[])?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "stored groq Bedrock override must succeed"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<(String, String)> = text.lines().map(status_row).collect::<Result<_, _>>()?;
    let matches: Vec<&(String, String)> = rows.iter().filter(|(id, _)| id == "groq").collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one groq row, no duplicate login/Bedrock rows: {text:?}"
    );
    assert_eq!(
        matches[0].1.split_whitespace().next(),
        Some("ready"),
        "stored groq Bedrock override must be ready: {text:?}"
    );
    Ok(())
}

#[test]
fn status_long_ids_keep_whitespace_token_boundaries() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    // Seeded canonical long id: `github-copilot` fills the padded id field,
    // so the fixed-width renderer glues it to the state column.
    fixture
        .write_auth(r#"{"github-copilot":{"kind":"api_key","key":"test-copilot-placeholder"}}"#)?;
    // Effective custom Bedrock long id: exercises the pending-row path with a
    // name well past the padded width.
    fixture.write_config(
        "[providers.custom-bedrock-longname]\napi = \"bedrock\"\nbase_url = \"http://127.0.0.1:9\"\nauth = \"none\"\n",
    )?;
    let output = bedrock_status(
        &fixture,
        &[
            ("AWS_ACCESS_KEY_ID", "test-key-id"),
            ("AWS_SECRET_ACCESS_KEY", "test-secret-key"),
        ],
    )?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "seeded copilot plus ready Bedrock must succeed"
    );
    let text = String::from_utf8_lossy(&output.stdout);
    // Token boundaries as humans/scripts consume them: split on whitespace,
    // never on format literals or space counts. Rows are located by id
    // substring with no order pin.
    let copilot = text
        .lines()
        .find(|line| line.contains("github-copilot"))
        .unwrap_or_else(|| panic!("github-copilot row missing: {text:?}"));
    assert!(
        copilot
            .split_whitespace()
            .eq(["github-copilot", "ready", "api_key"]),
        "long ready id must stay whitespace-separated: {copilot:?}"
    );
    let qwen = text
        .lines()
        .find(|line| line.contains("qwen-token-plan-cn"))
        .unwrap_or_else(|| panic!("qwen-token-plan-cn row missing: {text:?}"));
    assert!(
        qwen.split_whitespace()
            .eq(["qwen-token-plan-cn", "not", "configured"]),
        "long unconfigured id must stay whitespace-separated: {qwen:?}"
    );
    let bedrock = text
        .lines()
        .find(|line| line.contains("custom-bedrock-longname"))
        .unwrap_or_else(|| panic!("custom-bedrock-longname row missing: {text:?}"));
    assert!(
        bedrock
            .split_whitespace()
            .eq(["custom-bedrock-longname", "ready", "bedrock"]),
        "long Bedrock id must stay whitespace-separated: {bedrock:?}"
    );
    Ok(())
}
