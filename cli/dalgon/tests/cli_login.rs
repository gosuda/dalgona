//! Binary-boundary tests for provider login and status commands.

mod support;

use std::{error::Error, fs};

use dal_provider::login_providers;
use sonic_rs::{JsonValueTrait as _, Value};
use support::CliFixture;

/// The state column of a provider that holds no credential.
const NOT_CONFIGURED: &str = "not configured";

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

/// One `login status` row: the provider id, its state (`ready`, `expired`, or
/// `not configured`), and the detail after the state (the credential kind and
/// expiry date; empty for `not configured`).
///
/// Rows are read by column, never by padding, so the table can grow a
/// provider without changing what a row says.
#[derive(Debug)]
struct StatusRow {
    provider: String,
    state: String,
    detail: String,
}

fn status_rows(stdout: &[u8]) -> Result<Vec<StatusRow>, Box<dyn Error>> {
    std::str::from_utf8(stdout)?
        .lines()
        .map(|line| -> Result<StatusRow, Box<dyn Error>> {
            let (provider, rest) = line
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("status row has no state column: {line:?}"))?;
            let rest = rest.trim();
            let (state, detail) = match rest.strip_prefix(NOT_CONFIGURED) {
                Some(detail) => (NOT_CONFIGURED, detail),
                None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
            };
            Ok(StatusRow {
                provider: provider.to_owned(),
                state: state.to_owned(),
                detail: detail.trim().to_owned(),
            })
        })
        .collect()
}

/// Reads the rows of a `login status` stdout and checks what every run
/// shares: one row for each provider the CLI signs in to, once each, in the
/// provider table's display order. The table grows with each preset, so the
/// roster comes from the table rather than a literal; the table's own tests
/// pin which vendor rows it keeps.
fn rows_of_every_sign_in_provider(stdout: &[u8]) -> Result<Vec<StatusRow>, Box<dyn Error>> {
    let rows = status_rows(stdout)?;
    let expected: Vec<&str> = login_providers().iter().map(|def| def.id).collect();
    let listed: Vec<&str> = rows.iter().map(|row| row.provider.as_str()).collect();
    assert_eq!(
        listed, expected,
        "status lists every sign-in provider once, in display order"
    );
    Ok(rows)
}

fn row_of<'a>(rows: &'a [StatusRow], provider: &str) -> Result<&'a StatusRow, Box<dyn Error>> {
    rows.iter()
        .find(|row| row.provider == provider)
        .ok_or_else(|| format!("no status row for {provider}").into())
}

/// The provider the sign-in hint names. The hint must be exactly one line,
/// `Run dalgon login <provider> to sign in.`, so a user can copy the command.
fn sole_hint_provider(stderr: &[u8]) -> Result<String, Box<dyn Error>> {
    let text = std::str::from_utf8(stderr)?;
    let provider = text
        .strip_prefix("Run dalgon login ")
        .and_then(|rest| rest.strip_suffix(" to sign in.\n"))
        .filter(|provider| !provider.contains(char::is_whitespace))
        .ok_or_else(|| format!("stderr is not exactly one sign-in hint: {text:?}"))?;
    Ok(provider.to_owned())
}

fn assert_never_echoes(output: &[u8], secrets: &[&str], stream: &str) {
    let text = String::from_utf8_lossy(output);
    for secret in secrets {
        assert!(!text.contains(secret), "{stream} echoes a stored secret");
    }
}

#[test]
fn piped_api_key_is_stored_privately_without_echo() -> Result<(), Box<dyn Error>> {
    const KEY: &str = "secret-login-token";
    let fixture = CliFixture::new()?;
    let output = fixture.output_with_stdin(
        &["login", "anthropic", "--api-key"],
        format!("{KEY}\n").as_bytes(),
    )?;

    assert_eq!(output.status.code(), Some(0));
    // A piped login shows no prompt or progress, and never echoes the key.
    assert_eq!(output.stderr, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "Saved Anthropic credentials. Run dal to pick a model.\n"
    );

    // The key lands in the credential file under its provider, as an API key.
    let saved_text = fs::read_to_string(fixture.auth_file())?;
    let saved: Value = sonic_rs::from_str(&saved_text)?;
    assert_eq!(saved["anthropic"]["kind"].as_str(), Some("api_key"));
    assert_eq!(saved["anthropic"]["key"].as_str(), Some(KEY));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(fixture.auth_file())?.permissions().mode() & 0o777,
            0o600
        );
    }

    // Status then reports that provider ready and every other one unconfigured.
    let status = fixture.output(&["login", "status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(status.stderr, [] as [u8; 0]);
    assert_never_echoes(&status.stdout, &[KEY], "status stdout");
    let rows = rows_of_every_sign_in_provider(&status.stdout)?;
    let anthropic = row_of(&rows, "anthropic")?;
    assert_eq!(
        (anthropic.state.as_str(), anthropic.detail.as_str()),
        ("ready", "api_key")
    );
    for row in rows.iter().filter(|row| row.provider != "anthropic") {
        assert_eq!(
            row.state, NOT_CONFIGURED,
            "{} was never signed in",
            row.provider
        );
        assert_eq!(
            row.detail, "",
            "{} has no credential to describe",
            row.provider
        );
    }
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
    // `expires_at` 1790380800 is 2026-09-26T00:00:00Z, long past for any run.
    fixture.write_auth(
        r#"{"openai-codex":{"kind":"oauth","access_token":"access-secret","refresh_token":"refresh-secret","expires_at":1790380800,"id_token":"id-secret","account_id":"account-1"}}"#,
    )?;

    let output = fixture.output(&["login", "status"])?;

    // The only credential is expired, so nothing is ready.
    assert_eq!(output.status.code(), Some(1));
    let tokens = ["secret", "access-secret", "refresh-secret", "id-secret"];
    assert_never_echoes(&output.stdout, &tokens, "stdout");
    assert_never_echoes(&output.stderr, &tokens, "stderr");
    let rows = rows_of_every_sign_in_provider(&output.stdout)?;
    let codex = row_of(&rows, "openai-codex")?;
    assert_eq!(
        (codex.state.as_str(), codex.detail.as_str()),
        ("expired", "oauth, expires 2026-09-26")
    );
    for row in rows.iter().filter(|row| row.provider != "openai-codex") {
        assert_eq!(
            row.state, NOT_CONFIGURED,
            "{} was never signed in",
            row.provider
        );
    }
    let first_missing = rows
        .iter()
        .find(|row| row.state == NOT_CONFIGURED)
        .ok_or("status lists a provider with no credential")?;
    assert_eq!(sole_hint_provider(&output.stderr)?, first_missing.provider);

    Ok(())
}

#[test]
fn status_with_no_credentials_prints_the_first_login_hint() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let output = fixture.output(&["login", "status"])?;

    assert_eq!(output.status.code(), Some(1));
    let rows = rows_of_every_sign_in_provider(&output.stdout)?;
    for row in &rows {
        assert_eq!(
            row.state, NOT_CONFIGURED,
            "{} was never signed in",
            row.provider
        );
        assert_eq!(
            row.detail, "",
            "{} has no credential to describe",
            row.provider
        );
    }
    // The hint names the first provider the table lists.
    let first = rows.first().ok_or("status printed no rows")?;
    assert_eq!(sole_hint_provider(&output.stderr)?, first.provider);

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

/// A provider the CLI does not sign in to is a usage error before any key is
/// read or stored. A near spelling of a real provider is unknown too, so a typo
/// can never write a credential under a name no one meant.
#[test]
fn login_refuses_an_unknown_provider_and_stores_nothing() -> Result<(), Box<dyn Error>> {
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
        let output = fixture.output(&["login", provider, "--api-key"])?;

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
        assert!(
            !fixture.auth_file().exists(),
            "{provider:?} wrote a credential file"
        );
    }
    Ok(())
}

/// Empty input is refused with its own message, and so is input that is only
/// blanks: the first is "nothing was piped", the second reaches the host's
/// empty-key refusal. Neither may create a credential file.
#[test]
fn piped_blank_input_is_refused_and_stores_nothing() -> Result<(), Box<dyn Error>> {
    const NOTHING_PIPED: &str = "dalgon: no API key was piped: standard input is empty\nPipe the key: printf %s \"$KEY\" | dalgon login PROVIDER --api-key\n";
    const BLANK_KEY: &str = "dalgon: login failed: sign-in failed: the API key is empty.\nRun dalgon login PROVIDER and try again.\n";
    for (input, message) in [
        ("", NOTHING_PIPED),
        ("\n", NOTHING_PIPED),
        ("\r\n", NOTHING_PIPED),
        ("\n\n", NOTHING_PIPED),
        ("   \n", BLANK_KEY),
        ("\t\n", BLANK_KEY),
    ] {
        let fixture = CliFixture::new()?;
        let output =
            fixture.output_with_stdin(&["login", "anthropic", "--api-key"], input.as_bytes())?;

        assert_eq!(output.status.code(), Some(1), "{input:?}");
        assert_eq!(output.stdout, [] as [u8; 0], "{input:?}");
        assert_eq!(String::from_utf8(output.stderr)?, message, "{input:?}");
        assert!(
            !fixture.auth_file().exists(),
            "{input:?} wrote a credential file"
        );
    }
    Ok(())
}

/// A piped key forgives its trailing line ending and nothing else. A line break
/// or control character inside the key can never be sent as an HTTP header, so
/// saving it would report success and then fail at the first request with an
/// error that never names the key. It is refused before anything is stored, and
/// the refusal never echoes the key.
#[test]
fn piped_key_forgives_only_its_trailing_line_ending() -> Result<(), Box<dyn Error>> {
    for input in ["sk-key", "sk-key\n", "sk-key\r\n", "sk-key\n\n"] {
        let fixture = CliFixture::new()?;
        let output =
            fixture.output_with_stdin(&["login", "anthropic", "--api-key"], input.as_bytes())?;
        assert_eq!(output.status.code(), Some(0), "{input:?}");
        let saved: Value = sonic_rs::from_str(&fs::read_to_string(fixture.auth_file())?)?;
        assert_eq!(
            saved["anthropic"]["key"].as_str(),
            Some("sk-key"),
            "{input:?}"
        );
    }
    for input in [
        "sk-line1\nsk-line2\n",
        "sk-line1\r\nsk-line2",
        "sk-line1\nsk-line2",
        "sk-nul\u{0}key\n",
        "\u{1b}[31msk-esc\n",
        "sk-del\u{7f}key\n",
    ] {
        let fixture = CliFixture::new()?;
        let output =
            fixture.output_with_stdin(&["login", "anthropic", "--api-key"], input.as_bytes())?;

        assert_eq!(output.status.code(), Some(1), "{input:?} was accepted");
        assert_eq!(output.stdout, [] as [u8; 0], "{input:?}");
        assert_eq!(
            String::from_utf8(output.stderr)?,
            "dalgon: cannot read standard input: the key holds a line break or control character\nPipe the key: printf %s \"$KEY\" | dalgon login PROVIDER --api-key\n",
            "{input:?}"
        );
        assert!(
            !fixture.auth_file().exists(),
            "{input:?} wrote a credential file"
        );
    }
    Ok(())
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
