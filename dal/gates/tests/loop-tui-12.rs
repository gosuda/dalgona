// Copyright (c) Cognition Inc. and other dal contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Records a real PTY session as a transcript artifact and diffs its replayed
//! screen against the committed snapshot.

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
#[path = "support/transcript.rs"]
mod transcript;
#[path = "support/vt.rs"]
mod vt;

use std::{error::Error, time::Duration};

use pty::{PtyProcess, dalgon_command};
use support::TestDir;
use transcript::{Transcript, assert_snapshot};

#[test]
fn pty_transcript_replays_to_the_committed_snapshot() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["snapshot reply text"])?;
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    let mut record = Transcript::new();

    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    record.absorb(terminal.output());

    terminal.write(b"snapshot prompt\r")?;
    record.input(b"snapshot prompt\r");
    terminal.wait_for(b"snapshot reply text", Duration::from_secs(10))?;
    // Let the commit burst settle so the captured frame is the final state.
    terminal.collect_for(Duration::from_millis(150))?;
    record.absorb(terminal.output());

    let path = dir.path().join("session.transcript");
    record.save(&path)?;

    // The committed transcript itself must round-trip through the parser.
    let loaded = Transcript::load(&path)?;
    let replayed = loaded.replay(80, 24);
    // The live workspace row ends in the run's random tempdir; normalize
    // everything from the tempdir marker onward or every run drifts there.
    // The temp root itself is spelled per-platform (`/tmp` on Linux,
    // `$TMPDIR`/`private` forms on macOS), so fold it to a stable `/tmp`.
    let temp = std::env::temp_dir();
    let temp_canonical = temp.canonicalize().unwrap_or_else(|_| temp.clone());
    let rows: Vec<String> = replayed
        .screen_rows()
        .into_iter()
        .chain(replayed.scrollback_rows())
        .map(|row| {
            row.replace(temp_canonical.to_string_lossy().as_ref(), "/tmp")
                .replace(temp.to_string_lossy().as_ref(), "/tmp")
        })
        .map(|row| match row.find("dalgon-gates-") {
            Some(at) => format!("{}<workspace>", &row[..at]),
            None => row,
        })
        .filter(|row| !row.trim().is_empty())
        .collect();
    assert!(
        rows.iter().any(|row| row.contains("snapshot reply text")),
        "replayed transcript shows the scripted reply"
    );

    assert_snapshot("loop-tui-12", "settled-screen", &rows)?;

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}
