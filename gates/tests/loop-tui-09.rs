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
#[path = "support/vt.rs"]
mod vt;

use std::{
    error::Error,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command, dalgon_command_with_fixture, shell_quote};
use support::TestDir;
use vt::FollowedScreen;

const SPAWN: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(400);
const QUIT: Duration = Duration::from_secs(10);
/// One glyph no escape sequence contains, so its bytes reaching the output
/// after it was typed can only be the composer repainting it.
const MARKER: &str = "\u{2603}";

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;
type FixtureResult = Result<String, Box<dyn Error + Send + Sync>>;

/// Waits until the armed approval's action row is on screen.
fn wait_for_armed_approval(terminal: &mut PtyProcess, screen: &mut FollowedScreen) -> TestResult {
    terminal
        .wait_until("the armed approval answer keys", SPAWN, |output| {
            screen.shows(output, dal_tui::copy::ids::DIALOG_ACTIONS_SHORT)
        })
        .map_err(|error| format!("{error}\nscreen:\n{}", screen.visible()))?;
    Ok(())
}

/// Waits until the approval dialog is closed and the composer placeholder is
/// back on screen, which proves keys are admitted again.
fn wait_for_closed_approval(terminal: &mut PtyProcess, screen: &mut FollowedScreen) -> TestResult {
    terminal
        .wait_until("the approval dialog to close", SPAWN, |output| {
            screen.shows(output, dal_tui::copy::ids::COMPOSER_PLACEHOLDER)
                && !screen.shows(output, dal_tui::copy::ids::DIALOG_ACTIONS_SHORT)
        })
        .map_err(|error| format!("{error}\nscreen:\n{}", screen.visible()))?;
    Ok(())
}

fn exec_fixture(command: &str, reply: &str) -> FixtureResult {
    let command = sonic_rs::to_string(command)?;
    let reply = sonic_rs::to_string(reply)?;
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"call-exec\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-exec\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{command},\"timeout_seconds\":30}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":{reply}}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    ))
}

/// Proves the session survived hostile input and still takes keys: it has
/// not exited (a clean exit status would also pass `quit_cleanly`), and a
/// glyph typed now is painted back. A child that already died cannot repaint
/// it, even though the kernel echoes typed bytes once the child restored
/// cooked mode, so the exit check comes before and after the echo.
fn assert_session_responds(terminal: &mut PtyProcess) -> TestResult {
    if let Some(status) = terminal.exited()? {
        return Err(format!("dalgon exited with {status} during the hostile input").into());
    }
    let before = terminal.output().len();
    terminal.write(MARKER.as_bytes())?;
    terminal
        .wait_until(
            "the typed glyph repainted by the composer",
            SPAWN,
            |output| {
                output[before..]
                    .windows(MARKER.len())
                    .any(|window| window == MARKER.as_bytes())
            },
        )
        .map_err(|error| format!("{error}: the session stopped taking input"))?;
    if let Some(status) = terminal.exited()? {
        return Err(format!("dalgon exited with {status} after hostile input").into());
    }
    Ok(())
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

/// The width gate is `columns < 40`: 39 refuses with the measured width,
/// 40 and 41 start the TUI and quit cleanly.
#[test]
fn terminal_width_gate_is_exactly_forty_columns() -> TestResult {
    for columns in [39_u16, 40, 41] {
        let dir = TestDir::new()?;
        let mut command = dalgon_command(dir.path(), &["unused reply"])?;
        command.args(["--screen", "inline"]);
        let mut terminal = PtyProcess::spawn(&mut command, columns, 24)?;
        if columns < 40 {
            terminal.wait_for(
                format!("the terminal is {columns} columns wide").as_bytes(),
                SPAWN,
            )?;
            let status = terminal.wait_for_exit(QUIT)?;
            assert_eq!(
                status.code(),
                Some(1),
                "{columns} columns must exit 1: {status}"
            );
        } else {
            terminal
                .wait_for(b"enter send", SPAWN)
                .map_err(|error| format!("{columns} columns must start the TUI: {error}"))?;
            assert!(
                !String::from_utf8_lossy(terminal.output()).contains("columns wide"),
                "{columns} columns must not trip the width gate"
            );
            terminal.write(b"\x04")?;
            let status = terminal.wait_for_exit(QUIT)?;
            assert!(
                status.success(),
                "{columns} columns: dalgon exited {status}"
            );
        }
    }
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
    assert_session_responds(&mut terminal)?;
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
    if let Some(status) = terminal.exited()? {
        return Err(format!("Ctrl-D inside an open paste quit dalgon: {status}").into());
    }
    terminal.write(b"\x1b[201~")?;
    terminal.collect_for(SETTLE)?;
    assert_session_responds(&mut terminal)?;
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
    assert_session_responds(&mut terminal)?;
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
    let mut screen = FollowedScreen::new(100, 30);
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"run the command\r")?;
    wait_for_armed_approval(&mut terminal, &mut screen)?;
    terminal.write(b"\xff\x00\x08\x7f\x1b[A\x1b[Bx\x0b\x0c")?;
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "garbage keystrokes must never satisfy an approval dialog"
    );
    wait_for_armed_approval(&mut terminal, &mut screen)?;
    terminal.write(b"n")?;
    wait_for_closed_approval(&mut terminal, &mut screen)?;
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "a denied approval must not run the command"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn keys_typed_as_an_approval_opens_never_answer_it() -> TestResult {
    let dir = TestDir::new()?;
    let marker = dir.path().join("approved-by-type-ahead");
    let command_text = format!("touch {}", shell_quote(&marker));
    let fixture = exec_fixture(&command_text, "turn finished")?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    command.args(["--screen", "inline", "--approval", "ask"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    let mut screen = FollowedScreen::new(100, 30);
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"run the command\r")?;
    let locked = format!(
        "{} · {}",
        dal_tui::copy::ids::APPROVAL_ESC_DENIES,
        dal_tui::copy::ids::DIALOG_ARMING
    );
    let deadline = Instant::now() + SPAWN;
    while !screen.shows(terminal.output(), &locked) {
        assert!(
            !marker.exists(),
            "a key typed as the approval opened allowed the command"
        );
        assert!(
            !screen.shows(terminal.output(), dal_tui::copy::ids::DIALOG_ACTIONS_SHORT),
            "the approval armed before the locked answer keys were seen on screen"
        );
        assert!(
            Instant::now() < deadline,
            "the approval never showed locked answer keys"
        );
        terminal.write(b"y")?;
        terminal.collect_for(Duration::from_millis(5))?;
    }
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "keys typed while the approval was locked must not allow the command"
    );

    wait_for_armed_approval(&mut terminal, &mut screen)?;
    terminal.collect_for(SETTLE)?;
    assert!(
        !marker.exists(),
        "dropped keys must not be replayed once the approval is armed"
    );
    terminal.write(b"y")?;
    terminal.wait_for(b"turn finished", SPAWN)?;
    assert!(
        marker.exists(),
        "a key pressed after the armed hint answers the approval"
    );
    Ok(())
}

#[test]
fn input_during_working_turn_stays_sane() -> TestResult {
    let dir = TestDir::new()?;
    let started = dir.path().join("worked");
    let release = dir.path().join("release");
    let command_text = format!(
        "touch {started} && while [ ! -e {release} ] && [ -e {started} ]; do sleep 0.1; done",
        started = shell_quote(&started),
        release = shell_quote(&release),
    );
    let fixture = exec_fixture(&command_text, "turn finished")?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    command.args(["--screen", "inline", "--approval", "ask"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    let mut screen = FollowedScreen::new(100, 30);
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"go\r")?;
    wait_for_armed_approval(&mut terminal, &mut screen)?;
    terminal.write(b"y")?;
    terminal.wait_until("the held command to start", SPAWN, |_| started.exists())?;
    wait_for_closed_approval(&mut terminal, &mut screen)?;
    terminal.write(b"queued text")?;
    terminal
        .wait_until("the queued draft in the composer", SPAWN, |output| {
            screen.shows(output, "queued text")
        })
        .map_err(|error| format!("{error}\nscreen:\n{}", screen.visible()))?;
    terminal.write(b"\x1b[A\x1b[B\x1b[Z\x05\x19")?;
    std::fs::write(&release, b"")?;
    terminal.wait_for(b"turn finished", SPAWN)?;
    quit_cleanly(&mut terminal)?;
    assert!(started.exists(), "the approved command must have run");
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
    assert_session_responds(&mut terminal)?;
    quit_cleanly(&mut terminal)?;
    Ok(())
}
