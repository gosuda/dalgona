#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Verifies settled transcript rows stay frozen in the real terminal, the
//! fullscreen viewport scrolls and searches, and the transcript overlay
//! leaves the main screen exactly as it found it.

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
    // The commit burst keeps streaming briefly after the row text first
    // appears (status repaint, sync close); let it settle before marking
    // the byte boundary, or a slower scheduler can split the burst across
    // it and look like a re-print of settled text.
    terminal.collect_for(Duration::from_millis(150))?;
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

/// Answers the capability probe without confirming kitty keyboard encoding,
/// so the legacy key column is the working one.
fn probe_answers() -> &'static [u8] {
    b"\x1b[?2026;2$y\x1b[?2027;2$y\x1b]11;rgb:0000/0000/0000\x07\x1b[?1;2c"
}

/// Finds `needle` in the byte stream.
fn find_sequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Reports whether `bytes` carries any cell content: escape-sequence payload
/// bytes are terminal control traffic, not content.
fn printable(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x1b => {
                index += 1;
                match bytes.get(index) {
                    Some(b'[') => {
                        index += 1;
                        while bytes
                            .get(index)
                            .is_some_and(|byte| !(0x40..=0x7e).contains(byte))
                        {
                            index += 1;
                        }
                        index += 1;
                    }
                    Some(b']') => {
                        index += 1;
                        while bytes
                            .get(index)
                            .is_some_and(|byte| *byte != 0x07 && *byte != 0x1b)
                        {
                            index += 1;
                        }
                        index += if bytes.get(index) == Some(&0x1b) {
                            2
                        } else {
                            1
                        };
                    }
                    _ => index += 1,
                }
            }
            byte if byte >= 0x20 && byte != 0x7f => return true,
            _ => index += 1,
        }
    }
    false
}

#[test]
fn tui_fullscreen_session_scrolls_jumps_and_searches() -> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["first fullscreen response"])?;
    command.args(["--screen", "fullscreen"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(probe_answers())?;
    terminal.collect_for(Duration::from_millis(150))?;

    let output = terminal.output();
    let alt_enter =
        find_sequence(output, b"\x1b[?1049h").expect("the session enters the alternate screen");
    assert!(
        !printable(&output[..alt_enter]),
        "nothing reaches the main screen before the alternate screen opens"
    );
    assert_eq!(
        occurrences_of(output, b"\x1b[?1049h"),
        1,
        "the alternate screen opens exactly once"
    );

    terminal.write(b"first prompt\r")?;
    terminal.wait_for(b"first fullscreen response", Duration::from_secs(10))?;
    terminal.wait_for_count(b"enter send", 2, Duration::from_secs(10))?;
    terminal.collect_for(Duration::from_millis(150))?;

    // PageUp detaches follow and shows the stopped cue.
    terminal.write(b"\x1b[5~")?;
    terminal.wait_for(
        dal_tui::copy::ids::FOLLOW_STOPPED.as_bytes(),
        Duration::from_secs(5),
    )?;
    let mut detached = VtRecorder::new(80, 24);
    detached.feed(terminal.output());
    assert!(
        detached
            .row_containing("following stopped · press end to jump to the latest")
            .is_some()
    );

    // End re-attaches at the live edge and the cue clears.
    terminal.write(b"\x1b[F")?;
    terminal.collect_for(Duration::from_millis(150))?;
    let mut attached = VtRecorder::new(80, 24);
    attached.feed(terminal.output());
    assert!(attached.row_containing("following stopped").is_none());

    // F3 (the legacy search key) opens the filter row over the viewport top.
    terminal.write(b"\x1bOR")?;
    terminal.write(b"first")?;
    terminal.wait_for(b"/first", Duration::from_secs(5))?;
    terminal.collect_for(Duration::from_millis(150))?;
    let mut searching = VtRecorder::new(80, 24);
    searching.feed(terminal.output());
    assert!(
        searching
            .screen_rows()
            .iter()
            .any(|row| row.trim_end() == "/first · 2 hits"),
        "the filter row counts transcript matches: {:?}",
        searching.screen_rows()
    );

    // Enter jumps to the latest match, detaching again; Esc closes the row.
    terminal.write(b"\r")?;
    terminal.wait_for(
        dal_tui::copy::ids::FOLLOW_STOPPED.as_bytes(),
        Duration::from_secs(5),
    )?;
    terminal.write(b"\x1b")?;
    terminal.collect_for(Duration::from_millis(200))?;
    let mut closed = VtRecorder::new(80, 24);
    closed.feed(terminal.output());
    assert!(
        !closed
            .screen_rows()
            .iter()
            .any(|row| row.contains("/first")),
        "the filter row closes: {:?}",
        closed.screen_rows()
    );
    assert!(closed.row_containing("following stopped").is_some());

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");

    let output = terminal.output();
    assert_eq!(
        occurrences_of(output, b"\x1b[r\x1b[?1049l"),
        1,
        "the alternate screen pops exactly once, at the exit"
    );
    assert_eq!(occurrences_of(output, b"\x1b[?1049h"), 1);
    let mut run = VtRecorder::new(80, 24);
    run.feed(output);
    assert_eq!(run.erase_display_sequences(), 0, "fullscreen never erases");
    assert!(!run.sync_is_open(), "synchronized brackets stay balanced");
    assert!(run.sync_pairs() > 0);
    assert!(run.row_containing("first fullscreen response").is_some());
    Ok(())
}

#[test]
fn tui_transcript_overlay_repaints_the_main_screen_in_place()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["overlay response"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(
        dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(),
        Duration::from_secs(10),
    )?;
    terminal.write(probe_answers())?;
    terminal.collect_for(Duration::from_millis(150))?;
    terminal.write(b"overlay prompt\r")?;
    terminal.wait_for(b"overlay response", Duration::from_secs(10))?;
    terminal.wait_for_count(b"enter send", 2, Duration::from_secs(10))?;
    terminal.collect_for(Duration::from_millis(150))?;
    let mut before = VtRecorder::new(80, 24);
    before.feed(terminal.output());
    let main_grid = before.screen_rows();

    // Ctrl+T opens the read-only overlay on the alternate screen.
    terminal.write(b"\x14")?;
    terminal.wait_for(b"\x1b[?1049h", Duration::from_secs(5))?;
    terminal.write(b"\x1b[5~")?;
    terminal.collect_for(Duration::from_millis(100))?;
    terminal.write(b"\x1b[5~")?;
    terminal.wait_for(
        dal_tui::copy::ids::FOLLOW_STOPPED.as_bytes(),
        Duration::from_secs(5),
    )?;
    let mut overlay = VtRecorder::new(80, 24);
    overlay.feed(terminal.output());
    assert!(
        overlay
            .screen_rows()
            .iter()
            .any(|row| row.starts_with("dal · ")),
        "the overlay carries the fullscreen header: {:?}",
        overlay.screen_rows()
    );
    assert!(overlay.row_containing("overlay response").is_some());

    // Esc leaves; the live block repaints in place on the main screen.
    terminal.write(b"\x1b")?;
    terminal.wait_for(b"\x1b[r\x1b[?1049l", Duration::from_secs(5))?;
    terminal.collect_for(Duration::from_millis(300))?;
    let exit_stream = terminal.output();
    let alt_enter = find_sequence(exit_stream, b"\x1b[?1049h")
        .expect("the overlay entered the alternate screen");
    let alt_exit = find_sequence(exit_stream, b"\x1b[r\x1b[?1049l")
        .expect("the overlay left the alternate screen");
    assert!(alt_enter < alt_exit, "the overlay leaves cleanly");

    let mut after = VtRecorder::new(80, 24);
    after.feed(terminal.output());
    assert_eq!(after.screen_rows(), main_grid, "the main grid is untouched");
    assert_eq!(
        after.row_containing("overlay response"),
        before.row_containing("overlay response"),
        "the settled block keeps its row"
    );

    terminal.write(b"\x04")?;
    let status = terminal.wait_for_exit(Duration::from_secs(5))?;
    assert!(status.success(), "dalgon exited with {status}");
    Ok(())
}

fn occurrences_of(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}
