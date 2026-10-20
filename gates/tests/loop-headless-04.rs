//! Print mode refuses `ask` approval and names the flag.
#![expect(
    clippy::disallowed_methods,
    reason = "SC test invokes the real CLI boundary"
)]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, fs, path::PathBuf, process::Command};

use support::{TestDir, dalgon_binary};

#[test]
fn print_mode_denies_ask_and_names_approval_flag() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let config_dir = home.join(".config/dal");
    let data_dir = home.join(".local/share/dal");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(&data_dir)?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/dalgon/tests/fixtures/replay/loop-headless.jsonl");
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n[providers.scripted]\nfixture = {:?}\n",
            fixture.to_string_lossy()
        ),
    )?;
    let output = Command::new(dalgon_binary()?)
        .current_dir(&home)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--approval", "ask", "Read test.txt and patch it."])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("--approval"));
    Ok(())
}

/// Runs the scripted read-patch-read-exec prompt in print mode with
/// `configured` in `dal.toml` and `flag` on the command line, returning the
/// exit code and stderr.
fn print_with_approval(
    configured: &str,
    flag: &str,
) -> Result<(Option<i32>, String), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config_dir = home.join(".config/dal");
    fs::create_dir_all(&config_dir)?;
    fs::create_dir_all(home.join(".local/share/dal"))?;
    fs::create_dir_all(&workspace)?;
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../cli/dalgon/tests/fixtures");
    fs::write(workspace.join("test.txt"), "before\n")?;
    fs::copy(
        fixtures.join("process/grandchild.sh"),
        workspace.join("grandchild.sh"),
    )?;
    fs::write(
        config_dir.join("dal.toml"),
        format!(
            "model = \"openai-responses/gpt-6\"\napproval = \"{configured}\"\nedit_style = \"replace\"\n[providers.scripted]\nfixture = {:?}\n",
            fixtures
                .join("replay/loop-headless.jsonl")
                .to_string_lossy()
        ),
    )?;
    let output = Command::new(dalgon_binary()?)
        .current_dir(&workspace)
        .env_clear()
        .envs(support::captured_shell_vars())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .args(["-p", "--approval", flag, "Read test.txt and patch it."])
        .output()?;
    Ok((output.status.code(), String::from_utf8(output.stderr)?))
}

/// The command-line flag, not the config file, picks the approval mode, in
/// both directions and at the middle rung. Naming the same mode in both
/// places (the test above) cannot tell a flag that is applied from one that is
/// ignored.
#[test]
fn approval_flag_overrides_the_configured_mode_in_both_directions()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let (code, stderr) = print_with_approval("ask", "all")?;
    assert_eq!(
        code,
        Some(0),
        "--approval all must beat a configured ask: {stderr}"
    );
    assert_eq!(stderr, "", "approving everything leaves nothing to report");

    let (code, stderr) = print_with_approval("all", "ask")?;
    assert_eq!(
        code,
        Some(1),
        "--approval ask must beat a configured all: {stderr}"
    );
    assert!(
        stderr.contains("approval is ask and no terminal is attached"),
        "{stderr}"
    );

    let (code, stderr) = print_with_approval("ask", "edits")?;
    assert_eq!(code, Some(1), "edits still denies exec: {stderr}");
    assert!(
        stderr.contains("exec needs approval; print mode cannot ask. Rerun with --approval all."),
        "{stderr}"
    );
    assert!(
        !stderr.contains("approval is ask and no terminal is attached"),
        "--approval edits must not be treated as ask: {stderr}"
    );
    Ok(())
}
