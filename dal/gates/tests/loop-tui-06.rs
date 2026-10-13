#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Checks quitting cancels an active real host turn before it closes.

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
    path::Path,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command_with_fixture};
use support::TestDir;
use vt::VtRecorder;

#[test]
fn tui_quit_cancels_running_turn() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let started = dir.path().join("exec-started");
    let pid_file = dir.path().join("exec.pid");
    let command_text = format!(
        "echo $$ > {} && touch {} && exec sleep 30",
        shell_quote(&pid_file),
        shell_quote(&started)
    );
    let fixture = exec_fixture(&command_text)?;
    let mut command = dalgon_command_with_fixture(dir.path(), &fixture)?;
    command.args(["--screen", "inline", "--approval", "ask"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(b"run a long command\r")?;
    terminal.wait_for(
        dal_tui::copy::ids::DIALOG_ACTIONS_SHORT.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(b"y")?;
    wait_for_file(&started, Duration::from_secs(5))?;
    terminal.wait_for(
        dal_tui::copy::ids::STATE_WORKING.as_bytes(),
        Duration::from_secs(5),
    )?;

    terminal.write(b"\x04")?;
    terminal.wait_for(
        dal_tui::copy::ids::TURN_CANCELLED.as_bytes(),
        Duration::from_secs(10),
    )?;
    let status = terminal.wait_for_exit(Duration::from_secs(10))?;
    assert!(status.success(), "dalgon exited with {status}");
    let output = terminal.output();
    let cancelled_at = output
        .windows(dal_tui::copy::ids::TURN_CANCELLED.len())
        .position(|bytes| bytes == dal_tui::copy::ids::TURN_CANCELLED.as_bytes())
        .expect("the user-visible turn outcome is present");
    let exit_at = output
        .windows(b"dalgon: session".len())
        .position(|bytes| bytes == b"dalgon: session")
        .expect("the terminal is restored before the saved-session exit line");
    assert!(
        cancelled_at < exit_at,
        "Cancel outcome must precede host-close output"
    );

    let mut recorder = VtRecorder::new(100, 30);
    recorder.feed(output);
    assert_eq!(recorder.rows_containing("The turn stopped.").len(), 1);

    let pid: u32 = std::fs::read_to_string(pid_file)?.trim().parse()?;
    assert!(pid > 0);
    #[cfg(target_os = "linux")]
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => assert_eq!(
            stat.split_whitespace().nth(2),
            Some("Z"),
            "cancelled exec process {pid} must not still be running"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let _first_output_byte = terminal.output().first().unwrap();
    Ok(())
}

fn exec_fixture(command: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let command = sonic_rs::to_string(command)?;
    Ok(format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"call-exec\",\"name\":\"exec\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"call-exec\",\"name\":\"exec\",\"args\":{{\"kind\":\"parsed\",\"value\":{{\"command\":{command},\"timeout_seconds\":60}}}}}}]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n"
    ))
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn wait_for_file(path: &Path, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{} was not created by the approved exec", path.display()),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
