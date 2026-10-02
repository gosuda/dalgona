#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Verifies slash commands dispatch through the real host table.

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

use pty::{PtyProcess, dalgon_command};
use support::TestDir;

#[test]
fn tui_commands_route_through_host_command_table() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &[])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;

    for (name, summary, reply) in [
        ("tree", "navigate the session tree", "The session is empty"),
        (
            "fork",
            "new branch from a previous message",
            "There is no message to fork from",
        ),
        (
            "clone",
            "copy this session into a new one",
            "There is nothing to clone",
        ),
        (
            "compact",
            "summarize older context now",
            "There is nothing to compact yet",
        ),
        ("model", "pick the model", "Pick a model"),
    ] {
        terminal.write(format!("/{name}").as_bytes())?;
        terminal.wait_for(summary.as_bytes(), Duration::from_secs(5))?;
        terminal.write(b"\r")?;
        terminal.wait_for(reply.as_bytes(), Duration::from_secs(10))?;
    }

    terminal.write(b"\x1b")?;
    terminal.collect_for(Duration::from_millis(60))?;
    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    let first_byte = terminal.output().first().unwrap();
    assert_ne!(*first_byte, 0);
    let _output =
        std::str::from_utf8(terminal.output()).expect("terminal output bytes remain valid UTF-8");
    Ok(())
}
