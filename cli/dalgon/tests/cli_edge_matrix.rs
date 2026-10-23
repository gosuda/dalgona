//! Binary-boundary matrix for headless print, aliases, signals, plugin, and rules.
//!
//! Process-edge proofs. Every proof runs the
//! REAL built binary with scripted providers or synthetic inputs only: no
//! network, temp dirs for config/data roots.

#[expect(
    dead_code,
    reason = "fixture helpers are shared across binary test crates"
)]
mod support;

use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::Duration,
};

use support::CliFixture;

const MODEL_LINE: &str = "model = \"openai/gpt-6-luna\"";
const USAGE: &str = "{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}";

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One scripted turn streaming `text` and ending the turn.
fn text_step(text: &str) -> String {
    let escaped = json_escape(text);
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{escaped}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{USAGE}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

/// One scripted turn requesting an `exec` call, ending in `tool_use` so the
/// agent runs the tool and then consumes the next script step.
fn exec_step(command: &str) -> String {
    let escaped = json_escape(command);
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-1\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":\"{escaped}\",\"timeout_seconds\":60}}}}}}]}},{{\"type\":\"usage\",\"usage\":{USAGE}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    )
}

/// Writes a provider-script fixture under the temp data root.
fn write_script(
    fixture: &CliFixture,
    name: &str,
    contents: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let dir = fixture.data.join("dal");
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    fs::write(&path, contents)?;
    Ok(path)
}

/// Builds a `dal.toml` selecting `model` with the scripted global override.
fn scripted_config(extra: &str, fixture: &Path) -> String {
    let rendered = fixture.to_string_lossy().replace('\\', "\\\\");
    format!("{extra}\n\n[providers.scripted]\nfixture = \"{rendered}\"\n")
}

/// Runs an alternate binary alias with the fixture's isolated roots.
#[expect(
    clippy::disallowed_methods,
    reason = "test harness: the fixture launches the built dalgon binary alias and forwards its own PATH"
)]
fn output_with_bin(fixture: &CliFixture, bin: &str, args: &[&str]) -> io::Result<Output> {
    let mut command = Command::new(bin);
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .args(args)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .env("XDG_DATA_HOME", &fixture.data)
        .current_dir(fixture.cwd());
    command.output()
}

/// Spawns the real binary with caller-owned stdio for signal proofs.
#[expect(
    clippy::disallowed_methods,
    reason = "test harness: the fixture spawns the built dalgon binary alias and forwards its own PATH"
)]
fn spawn_with_bin(fixture: &CliFixture, bin: &str, args: &[&str]) -> io::Result<Child> {
    let mut command = Command::new(bin);
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
        .args(args)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .env("XDG_DATA_HOME", &fixture.data)
        .current_dir(fixture.cwd())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.spawn()
}

#[expect(
    clippy::disallowed_methods,
    reason = "test harness: signals the spawned binary under test"
)]
fn send_signal(pid: u32, sig: &str) {
    let _ = Command::new("kill").arg(sig).arg(pid.to_string()).output();
}

fn write_plugin(fixture: &CliFixture, name: &str, star: &str) -> Result<(), Box<dyn Error>> {
    let dir = fixture.data.join("dal").join("plugins").join(name);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("plugin.star"), star)?;
    Ok(())
}

const FOCUS_STAR: &str = "load(\"@dal/v1\", \"dal\")\n\ndef hello(ctx, args):\n    return \"hi\"\n\nhello_tool = dal.tool(\n    description = \"Say hi.\",\n    input = dal.schema(),\n    uses = [\"tools.read\"],\n    run = hello,\n)\n\nplugin = dal.plugin(\n    name = \"focus\",\n    version = \"0.1.0\",\n    tools = {\"hello\": hello_tool},\n)\n";

const ALPHA_STAR: &str = "load(\"@dal/v1\", \"dal\")\n\nplugin = dal.plugin(\n    name = \"alpha\",\n    version = \"0.1.0\",\n)\n";

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// --- Headless print mode ----------------------------------------------------

#[test]
fn headless_print_streams_assistant_text_with_single_trailing_lf() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let script = write_script(
        &fixture,
        "print.jsonl",
        &text_step("hello from scripted headless"),
    )?;
    fixture.write_config(&scripted_config(MODEL_LINE, &script))?;

    let output = fixture.output(&["--print", "say hi"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, b"hello from scripted headless\n");
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn headless_json_prints_single_acp_result_line() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let script = write_script(
        &fixture,
        "json.jsonl",
        &text_step("hello from scripted headless"),
    )?;
    fixture.write_config(&scripted_config(MODEL_LINE, &script))?;

    let output = fixture.output(&["--json", "say hi"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        output.stdout,
        b"{\"jsonrpc\":\"2.0\",\"id\":null,\"result\":{\"stopReason\":\"end_turn\"}}\n"
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn headless_print_without_model_is_requested_failure_with_fix() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["--print", "hi"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: no model is configured: dal.toml has no model key and --model was not given\nRun dalgon once and pick a model after sign-in, or pass --model ID. See dalgon models.\n"
    );
    Ok(())
}

#[test]
fn headless_print_denial_reports_per_call_notes_and_summary_count() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let script = write_script(
        &fixture,
        "denial.jsonl",
        &[exec_step("echo hi"), text_step("done")].concat(),
    )?;
    fixture.write_config(&scripted_config(MODEL_LINE, &script))?;

    let output = fixture.output(&["--print", "run it"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}",
        stdout_text(&output)
    );
    assert!(
        stdout_text(&output).contains("done"),
        "the follow-up text streams before the denial short-circuit: {}",
        stdout_text(&output)
    );
    assert_eq!(
        output.stderr,
        b"dalgon: approval is ask and no terminal is attached: patch and exec calls are denied. Re-run with --approval all to approve them.\ndalgon: exec needs approval; print mode cannot ask. Rerun with --approval all.\ndalgon: 1 call denied by approval mode.\n"
    );
    Ok(())
}

#[test]
fn connect_with_print_is_usage_with_two_line_diagnostic() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["--print", "--connect", "ws://127.0.0.1:1/x", "hi"])?;
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: --connect requires the interactive TUI\nRun dalgon without --print or --json, or omit --connect.\n"
    );
    Ok(())
}

#[test]
fn continue_with_resume_is_usage() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["--continue", "--resume"])?;
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: --continue and --resume cannot be used together\nPass one of --continue or --resume, not both.\n"
    );
    Ok(())
}

#[test]
fn empty_name_is_usage() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["--name", ""])?;
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: --name cannot be empty\nPass a session name, or omit --name.\n"
    );
    Ok(())
}

// --- Aliases ------------------------------------------------------------------

#[test]
fn alias_binaries_report_the_same_package_version() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let package = env!("CARGO_PKG_VERSION");

    let dalgon = output_with_bin(&fixture, env!("CARGO_BIN_EXE_dalgon"), &["--version"])?;
    let dal = output_with_bin(&fixture, env!("CARGO_BIN_EXE_dal"), &["--version"])?;
    let dl = output_with_bin(&fixture, env!("CARGO_BIN_EXE_dl"), &["--version"])?;

    for (name, output) in [("dalgon", &dalgon), ("dal", &dal), ("dl", &dl)] {
        assert_eq!(output.status.code(), Some(0), "{name} --version failed");
        assert_eq!(output.stderr, [] as [u8; 0], "{name} leaked to stderr");
    }
    // All three aliases bake the same factory with binary "dalgon", so every
    // alias reports the identical version line: same tree, same version.
    let expected = format!("dalgon {package}\n");
    assert_eq!(stdout_text(&dalgon), expected);
    assert_eq!(stdout_text(&dal), expected);
    assert_eq!(stdout_text(&dl), expected);
    Ok(())
}

#[test]
fn alias_binaries_share_help_tree_and_config_behavior() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    for (name, bin) in [
        ("dalgon", env!("CARGO_BIN_EXE_dalgon")),
        ("dal", env!("CARGO_BIN_EXE_dal")),
        ("dl", env!("CARGO_BIN_EXE_dl")),
    ] {
        let output = output_with_bin(&fixture, bin, &["--help"])?;
        assert_eq!(output.status.code(), Some(0), "{name} --help failed");
        let text = stdout_text(&output);
        for subcommand in ["plugin", "rules", "serve", "docs", "completion", "login"] {
            assert!(
                text.contains(subcommand),
                "{name} help is missing {subcommand}"
            );
        }
    }

    // Same config roots and family: an identical empty config lists the same
    // empty plugin set through every alias.
    for (name, bin) in [
        ("dalgon", env!("CARGO_BIN_EXE_dalgon")),
        ("dal", env!("CARGO_BIN_EXE_dal")),
        ("dl", env!("CARGO_BIN_EXE_dl")),
    ] {
        let output = output_with_bin(&fixture, bin, &["plugin", "list"])?;
        assert_eq!(output.status.code(), Some(0), "{name} plugin list failed");
        assert_eq!(
            output.stdout, b"No plugins configured.\n",
            "{name} diverged"
        );
        assert_eq!(output.stderr, [] as [u8; 0], "{name} leaked to stderr");
    }
    Ok(())
}

// --- Signals ------------------------------------------------------------------

#[test]
#[cfg_attr(not(unix), ignore = "POSIX SIGINT delivery only")]
fn sigint_during_headless_exec_turn_exits_130() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let script = write_script(
        &fixture,
        "long.jsonl",
        &[exec_step("sleep 30"), text_step("done")].concat(),
    )?;
    fixture.write_config(&scripted_config(
        "model = \"openai/gpt-6-luna\"\napproval = \"all\"",
        &script,
    ))?;

    let child = spawn_with_bin(&fixture, env!("CARGO_BIN_EXE_dalgon"), &["--print", "go"])?;
    let pid = child.id();
    // If the cancel path ever wedges, do not hang the suite: SIGKILL the
    // child well after the SIGINT under test should have landed.
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(60));
        send_signal(pid, "-KILL");
    });
    // Host startup plus the scripted tool round-trip settle locally in well
    // under a second; the 30s exec keeps the turn in flight for the signal.
    thread::sleep(Duration::from_secs(4));
    send_signal(pid, "-INT");
    let output = child.wait_with_output()?;

    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The interrupt lands before any model text streams: the exec step carries
    // no text deltas and the follow-up step never runs.
    assert_eq!(output.stdout, [] as [u8; 0]);
    Ok(())
}

// --- Plugin CLI ---------------------------------------------------------------

#[test]
fn plugin_list_with_no_plugins_is_deterministic_empty() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["plugin", "list"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(output.stdout, b"No plugins configured.\n");
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn plugin_list_sorts_by_name_bytes() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    write_plugin(&fixture, "focus", FOCUS_STAR)?;
    write_plugin(&fixture, "alpha", ALPHA_STAR)?;
    // Config order is deliberately unsorted: the list must still byte-sort.
    fixture.write_config("plugins = [\"focus\", \"alpha\"]\n")?;

    let output = fixture.output(&["plugin", "list"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let text = stdout_text(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "expected two plugin rows: {lines:?}");
    assert!(
        lines[0].starts_with("alpha (user):"),
        "first row: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("focus (user):"),
        "second row: {}",
        lines[1]
    );
    assert!(
        lines[1].contains("persistent grant: not granted"),
        "{}",
        lines[1]
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn plugin_grant_revoke_roundtrip_is_idempotent_with_counts() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    write_plugin(&fixture, "focus", FOCUS_STAR)?;
    fixture.write_config("plugins = [\"focus\"]\n")?;

    let grant = fixture.output(&["plugin", "grant", "focus"])?;
    assert_eq!(
        grant.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&grant)
    );
    assert!(
        stdout_text(&grant).starts_with("Granted \"focus\" (user):"),
        "{}",
        stdout_text(&grant)
    );

    let listed = fixture.output(&["plugin", "list"])?;
    assert_eq!(listed.status.code(), Some(0));
    assert!(
        stdout_text(&listed).contains("; persistent grant: granted\n"),
        "{}",
        stdout_text(&listed)
    );
    assert!(
        !stdout_text(&listed).contains("not granted"),
        "{}",
        stdout_text(&listed)
    );

    let again = fixture.output(&["plugin", "grant", "focus"])?;
    assert_eq!(again.status.code(), Some(0));
    assert!(
        stdout_text(&again).starts_with("Already granted \"focus\""),
        "{}",
        stdout_text(&again)
    );

    let revoke = fixture.output(&["plugin", "revoke", "focus"])?;
    assert_eq!(revoke.status.code(), Some(0));
    assert!(
        stdout_text(&revoke).contains("Revoked \"focus\": removed 1 persistent grant(s)."),
        "{}",
        stdout_text(&revoke)
    );

    let unlisted = fixture.output(&["plugin", "list"])?;
    assert_eq!(unlisted.status.code(), Some(0));
    assert!(
        stdout_text(&unlisted).contains("persistent grant: not granted"),
        "{}",
        stdout_text(&unlisted)
    );

    let empty = fixture.output(&["plugin", "revoke", "focus"])?;
    assert_eq!(empty.status.code(), Some(0));
    assert!(
        stdout_text(&empty).contains("No persistent grants for \"focus\"."),
        "{}",
        stdout_text(&empty)
    );
    Ok(())
}

#[test]
fn plugin_grant_fails_closed_for_unknown_and_builtin() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let unknown = fixture.output(&["plugin", "grant", "nope"])?;
    assert_eq!(unknown.status.code(), Some(1));
    assert_eq!(unknown.stdout, [] as [u8; 0]);
    assert!(
        stderr_text(&unknown).contains("plugin \"nope\" is not configured"),
        "{}",
        stderr_text(&unknown)
    );

    let builtin = fixture.output(&["plugin", "grant", "prompt"])?;
    assert_eq!(builtin.status.code(), Some(1));
    assert_eq!(builtin.stdout, [] as [u8; 0]);
    assert!(
        stderr_text(&builtin).contains("is a builtin extension: it needs no grant"),
        "{}",
        stderr_text(&builtin)
    );
    Ok(())
}

// --- Rules CLI ----------------------------------------------------------------

#[test]
fn rules_with_no_rules_names_both_roots() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["rules"])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    let expected = format!(
        "No rules. Add Markdown files to {} or {}.\n",
        fixture.data.join("dal").join("rules").display(),
        fixture.cwd().join(".dal").join("rules").display()
    );
    assert_eq!(stdout_text(&output), expected);
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn rules_with_skipped_file_fails_with_problem_list() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let user_rules = fixture.data.join("dal").join("rules");
    fs::create_dir_all(&user_rules)?;
    fs::write(
        user_rules.join("broken.md"),
        "---\ncondition: git commit -m\n",
    )?;

    let output = fixture.output(&["rules"])?;
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {}",
        stdout_text(&output)
    );
    assert!(
        stdout_text(&output).contains("problems (1)"),
        "{}",
        stdout_text(&output)
    );
    assert!(
        stdout_text(&output).contains("broken.md"),
        "{}",
        stdout_text(&output)
    );
    Ok(())
}

#[test]
fn rules_test_with_no_match_reports_count_and_fails() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["rules", "test", "ordinary prose with no match"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"No rule fired. Checked 0 stream rules.\n");
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn rules_test_tool_combo_without_tool_source_is_usage() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;

    let output = fixture.output(&["rules", "test", "--tool", "exec", "hi"])?;
    assert_eq!(output.status.code(), Some(124));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: --tool and --path require --source tool\nPass --source tool, or omit --tool and --path.\n"
    );
    Ok(())
}

#[test]
fn rules_test_fires_workspace_tool_rule() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let project_rules = fixture.cwd().join(".dal").join("rules");
    fs::create_dir_all(&project_rules)?;
    fs::write(
        project_rules.join("no-auto-commit.md"),
        "---\ncondition: git commit -m\nscope: tool:exec\n---\nDo not commit automatically.\n",
    )?;

    let output = fixture.output(&[
        "rules",
        "test",
        "--source",
        "tool",
        "--tool",
        "exec",
        "git commit -m x",
    ])?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        stderr_text(&output)
    );
    assert_eq!(
        output.stdout,
        b"fired: no-auto-commit (interrupt). The `exec` call matched /git commit -m/.\n"
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn rules_test_stdin_over_limit_fails_closed() -> Result<(), Box<dyn Error>> {
    let fixture = CliFixture::new()?;
    let oversized = vec![b'x'; 1_048_576 + 1];

    let output = fixture.output_with_stdin(&["rules", "test", "-"], &oversized)?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"dalgon: rules test input exceeds 1048576 bytes\nPass TEXT directly or pipe at most 1048576 bytes to dalgon rules test.\n"
    );
    Ok(())
}
