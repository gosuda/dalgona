#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Verifies settled transcript rows stay frozen in the real terminal.

#[path = "support/pty.rs"]
#[expect(
    dead_code,
    reason = "PTY support helpers are shared across TUI gate targets"
)]
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

#[test]
fn tui_pty_keeps_settled_rows_frozen() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(
        dir.path(),
        &["first frozen response", "second live response"],
    )?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal
        .write(b"\x1b[?2026;1$y\x1b[?2027;1$y\x1b[?1u\x1b]11;rgb:0000/0000/0000\x07\x1b[?1;2c")?;
    terminal.collect_for(Duration::from_millis(20))?;

    terminal.write(b"first prompt\r")?;
    terminal.wait_for(b"first frozen response", Duration::from_secs(10))?;
    terminal.wait_for_count(b"enter send", 2, Duration::from_secs(10))?;
    let mut first_frame = VtRecorder::new(80, 24);
    first_frame.feed(terminal.output());
    let committed_prompt_before = first_frame.row_containing("first prompt").unwrap();
    let committed_before = first_frame.row_containing("first frozen response").unwrap();
    let committed_output_end = terminal.output().len();

    terminal.write(b"second prompt\r")?;
    terminal.wait_for(b"second live response", Duration::from_secs(10))?;
    terminal.wait_for_count(b"enter send", 3, Duration::from_secs(10))?;
    let mut settled = VtRecorder::new(80, 24);
    settled.feed(terminal.output());
    let committed_prompt_after = settled.row_containing("first prompt").unwrap();
    let committed_after = settled
        .row_containing("first frozen response")
        .expect("the earlier response remains visible after the next turn settles");

    assert_eq!(committed_prompt_after, committed_prompt_before);
    assert_eq!(committed_after, committed_before);
    let followup_bytes = terminal.output().get(committed_output_end..).unwrap();
    assert!(
        !followup_bytes
            .windows(b"first prompt".len())
            .any(|bytes| bytes == b"first prompt")
    );
    assert!(
        !followup_bytes
            .windows(b"first frozen response".len())
            .any(|bytes| bytes == b"first frozen response")
    );
    assert_eq!(settled.erase_display_sequences(), 0);
    assert!(!settled.sync_is_open());
    assert!(settled.sync_pairs() > 0);
    assert!(settled.row_containing("second live response").is_some());
    assert!(
        std::str::from_utf8(terminal.output())
            .expect("TUI output remains valid UTF-8")
            .contains("first frozen response")
    );

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}
