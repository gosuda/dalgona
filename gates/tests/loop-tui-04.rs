#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
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
#[path = "support/vt.rs"]
mod vt;

use std::{error::Error, time::Duration};

use pty::{PtyProcess, dalgon_command};
use support::TestDir;
use vt::VtRecorder;

const COLUMNS: u16 = 100;
const ROWS: u16 = 30;

#[test]
fn tui_commands_route_through_host_command_table() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &[])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, COLUMNS, ROWS)?;
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
    let composer = format!("> {}", dal_tui::copy::ids::COMPOSER_PLACEHOLDER);
    let mut screen = VtRecorder::new(COLUMNS, ROWS);
    let mut consumed = 0;
    let closed = terminal.wait_until(
        "the composer after Esc closed the model picker",
        Duration::from_secs(10),
        |output| {
            screen.feed(&output[consumed..]);
            consumed = output.len();
            let rows = screen.screen_rows();
            !screen.sync_is_open()
                && rows.iter().any(|row| row.trim_end() == composer)
                && !rows.iter().any(|row| row.contains("Pick a model"))
        },
    );
    if let Err(error) = closed {
        let rows = screen
            .screen_rows()
            .iter()
            .map(|row| row.trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("{error}\nscreen:\n{rows}").into());
    }
    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    assert_eq!(terminal.terminal_restore_problem()?, None);
    let first_byte = terminal.output().first().unwrap();
    assert_ne!(*first_byte, 0);
    let _output =
        std::str::from_utf8(terminal.output()).expect("terminal output bytes remain valid UTF-8");
    Ok(())
}
