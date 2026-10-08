#![expect(
    dead_code,
    reason = "gate support exposes helpers shared across independent targets"
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Measures the first real PTY frame before terminal probe replies.

#[expect(
    dead_code,
    reason = "PTY support includes helpers used by other gate targets"
)]
#[path = "support/pty.rs"]
mod pty;
mod support;

use std::{
    error::Error,
    time::{Duration, Instant},
};

use pty::{PtyProcess, dalgon_command};
use support::TestDir;

#[test]
#[ignore = "nightly idle-machine timing budget"]
fn tui_first_frame_within_one_second() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["provider must not run before input"])?;
    command.args(["--screen", "inline"]);
    let started = Instant::now();
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    let probe_deadline = started + Duration::from_millis(200);
    let placeholder = dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes();
    let mut frame_before_probe_reply = false;

    while Instant::now() < probe_deadline {
        terminal.collect_for(Duration::from_millis(5))?;
        if terminal
            .output()
            .windows(placeholder.len())
            .any(|bytes| bytes == placeholder)
        {
            frame_before_probe_reply = true;
            break;
        }
    }
    let reply_time = Instant::now();
    terminal
        .write(b"\x1b[?2026;1$y\x1b[?2027;1$y\x1b[?1u\x1b]11;rgb:0000/0000/0000\x07\x1b[?1;2c")?;
    terminal.wait_for(placeholder, Duration::from_secs(1))?;
    let first_frame = Instant::now();

    assert!(first_frame.duration_since(started) < Duration::from_secs(1));
    assert!(
        frame_before_probe_reply,
        "the first frame must paint before the delayed probe reply at {reply_time:?}"
    );
    let _verified_frame = terminal
        .output()
        .windows(placeholder.len())
        .find(|bytes| *bytes == placeholder)
        .expect("the first frame contains its composer placeholder");
    let first_byte = terminal.output().first().unwrap();
    assert_ne!(*first_byte, 0);
    assert!(
        !terminal
            .output()
            .windows(b"provider must not run before input".len())
            .any(|bytes| bytes == b"provider must not run before input")
    );

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}
