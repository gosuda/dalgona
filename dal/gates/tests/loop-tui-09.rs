#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Adversarial terminal input: hostile byte streams, boundary geometries, and
//! provider failures must produce descriptive errors or be safely ignored, and
//! the session must stay quittable. A crash, hang, or silent approval is a
//! failure.

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

use std::{error::Error, time::Duration};

use pty::{PtyProcess, dalgon_command, dalgon_command_with_fixture};
use support::TestDir;

const SPAWN: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(400);
const QUIT: Duration = Duration::from_secs(10);

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type FixtureResult = Result<String, Box<dyn Error + Send + Sync>>;

fn exec_fixture(command: &str, reply: &str) -> FixtureResult {
    let command = sonic_rs::to_string(command)?;
    let reply = sonic_rs::to_string(reply)?;
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"call-exec\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-exec\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{command},\"timeout_seconds\":30}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{reply}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    ))
}

/// Quits after hostile input: submit drains any composer text (Ctrl-D only
/// quits on an empty composer and composer editing keys are not bound), then
/// Ctrl-D quits. Retries while the child drains the injected flood.
fn quit_cleanly(terminal: &mut PtyProcess) -> TestResult {
    terminal.write(b"\r")?;
    terminal.collect_for(SETTLE)?;
    for _ in 0..4 {
        terminal.write(b"\x04")?;
        match terminal.wait_for_exit(Duration::from_secs(5)) {
            Ok(status) => {
                assert!(
                    status.success(),
                    "dalgon did not exit cleanly after hostile input: {status}"
                );
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("dalgon stayed alive after repeated submit/Ctrl-D quit attempts".into())
}

#[test]
fn narrow_terminal_fails_descriptively() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["unused reply"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 30, 24)?;
    terminal.wait_for(b"the terminal is 30 columns wide", SPAWN)?;
    let status = terminal.wait_for_exit(QUIT)?;
    assert_eq!(
        status.code(),
        Some(1),
        "narrow terminal must exit 1: {status}"
    );
    Ok(())
}

#[test]
fn dumb_term_fails_descriptively() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["unused reply"])?;
    command.env("TERM", "dumb").args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(b"TERM is \"dumb\"", SPAWN)?;
    let status = terminal.wait_for_exit(QUIT)?;
    assert_eq!(status.code(), Some(1), "dumb TERM must exit 1: {status}");
    Ok(())
}

#[test]
fn binary_garbage_input_keeps_session_alive() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["ignored"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"\xff\xfe\x00\x01\x02\x0b\x0c\x0e\x0f\x1b[31mINJ\x1b[0m\xff\xe6")?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[Z\x1b[5~\x1b[6~\x1bOH\x1bOF")?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x03")?;
    terminal.collect_for(SETTLE)?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn unterminated_paste_does_not_trap_quit() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["ignored"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"\x1b[200~")?;
    terminal.write(&[b'x'; 4 * 1024])?;
    terminal.collect_for(SETTLE)?;
    // Ctrl-D inside an open paste is literal paste text, never a quit.
    terminal.write(b"\x04")?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x1b[201~")?;
    terminal.collect_for(SETTLE)?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn resize_storm_keeps_session_alive() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["ignored"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    for (columns, rows) in [
        (120_u16, 40_u16),
        (80, 24),
        (41, 10),
        (40, 8),
        (39, 24),
        (20, 5),
        (1, 1),
        (100, 30),
    ] {
        terminal.resize(columns, rows)?;
        terminal.write(b"z")?;
        terminal.collect_for(Duration::from_millis(50))?;
    }
    for _ in 0..40 {
        terminal.resize(40, 24)?;
        terminal.resize(120, 40)?;
    }
    terminal.resize(100, 30)?;
    terminal.collect_for(SETTLE)?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn approval_dialog_ignores_garbage_keystrokes() -> TestResult {
    let dir = TestDir::new()?;
    let marker = dir.path().join("must-not-exist");
    let command_text = format!("touch {}", shell_quote(&marker));
    let fixture = exec_fixture(&command_text, "done")?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    command.args(["--screen", "inline", "--approval", "ask"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"run the command\r")?;
    terminal.wait_for(b"Allow this command?", SPAWN)?;
    terminal.write(b"\xff\x00\x08\x7f\x1b[A\x1b[Bx\x0b\x0c")?;
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "garbage keystrokes must never satisfy an approval dialog"
    );
    terminal.write(b"n")?;
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "a denied approval must not run the command"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn input_during_working_turn_stays_sane() -> TestResult {
    let dir = TestDir::new()?;
    let marker = dir.path().join("worked");
    let command_text = format!("touch {}", shell_quote(&marker));
    let fixture = exec_fixture(&command_text, "turn finished")?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    command.args(["--screen", "inline", "--approval", "ask"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"go\r")?;
    terminal.wait_for(b"Allow this command?", SPAWN)?;
    terminal.write(b"y")?;
    terminal.wait_for(dal_tui::copy::ids::STATE_WORKING.as_bytes(), SPAWN)?;
    terminal.write(b"queued text\x1b[A\x1b[B\x1b[Z\x05\x19")?;
    terminal.wait_for(b"turn finished", SPAWN)?;
    quit_cleanly(&mut terminal)?;
    assert!(marker.exists(), "the approved command must have run");
    Ok(())
}

#[test]
fn exhausted_script_reports_descriptive_error() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &[])?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"work\r")?;
    terminal.wait_for(b"script exhausted", SPAWN)?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn corrupt_replay_line_reports_descriptive_error() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command_with_fixture(dir.path(), "{not json\n")?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    // A corrupt fixture is rejected before the TUI draws: the child must
    // print the descriptive two-line error and exit 1.
    terminal.wait_for(b"script replay line 1", SPAWN)?;
    let status = terminal.wait_for_exit(QUIT)?;
    assert_eq!(
        status.code(),
        Some(1),
        "corrupt fixture must exit 1: {status}"
    );
    Ok(())
}

#[test]
fn one_huge_paste_does_not_hang() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["ignored"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let mut paste = Vec::with_capacity(300 * 1024);
    paste.extend_from_slice(b"\x1b[200~");
    paste.resize(256 * 1024, b'z');
    paste.extend_from_slice(b"\x1b[201~");
    terminal.write(&paste)?;
    terminal.collect_for(Duration::from_secs(2))?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
