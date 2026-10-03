#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Korean and Hangul correctness on the real terminal grid: decomposed jamo
//! (NFD) rendering, IME-style incremental input and mid-cluster backspace,
//! compatibility and halfwidth jamo widths, and ambiguous-width behavior
//! under a Korean locale. A torn syllable, a stray jamo left by backspace, or
//! a zero-width glyph is a failure.

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

use pty::{PtyProcess, dalgon_command};
use support::TestDir;
use vt::VtRecorder;

const SPAWN: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(400);

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

/// Submits whatever the composer holds, then quits with Ctrl-D.
fn quit_cleanly(terminal: &mut PtyProcess) -> TestResult {
    terminal.write(b"\r")?;
    terminal.collect_for(SETTLE)?;
    for _ in 0..4 {
        terminal.write(b"\x04")?;
        match terminal.wait_for_exit(Duration::from_secs(5)) {
            Ok(status) => {
                assert!(
                    status.success(),
                    "dalgon did not exit cleanly after Hangul input: {status}"
                );
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("dalgon stayed alive after repeated submit/Ctrl-D quit attempts".into())
}

/// Types `text`, submits it, waits for the scripted reply, then waits for the
/// status row to idle. A mid-turn capture can hold the reply twice — once in
/// committed transcript rows and once in the live block's assistant text — so
/// row counts are only stable once the turn state clears.
fn prompt_and_remember(
    terminal: &mut PtyProcess,
    text: &str,
    reply: &str,
) -> Result<VtRecorder, Box<dyn Error + Send + Sync>> {
    terminal.write(text.as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\r")?;
    terminal.wait_for(reply.as_bytes(), SPAWN)?;
    let deadline = Instant::now() + SPAWN;
    // Busy markers can scroll off before the live block clears, so also
    // require the output stream to go quiet: three consecutive polls with
    // no new bytes and no busy status mean the frame is settled.
    let mut last_len = 0usize;
    let mut stable = 0usize;
    loop {
        terminal.collect_for(Duration::from_millis(60))?;
        let output = terminal.output();
        let mut probe = VtRecorder::new(100, 30);
        probe.feed(output);
        let busy = probe.screen_rows().iter().any(|row| {
            let row = row.trim_start();
            row.starts_with("* ") || row.contains(dal_tui::copy::ids::STATE_WAITING)
        });
        if !busy && output.len() == last_len {
            stable += 1;
            if stable >= 3 {
                return Ok(probe);
            }
        } else {
            stable = 0;
            last_len = output.len();
        }
        if Instant::now() >= deadline {
            return Ok(probe);
        }
    }
}

/// The full screen plus scrollback as one searchable text.
fn all_text(recorder: &VtRecorder) -> String {
    recorder
        .screen_rows()
        .into_iter()
        .chain(recorder.scrollback_rows())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn decomposed_jamo_reply_wraps_whole_syllables() -> TestResult {
    let dir = TestDir::new()?;
    // 50 fully-decomposed syllables (150 codepoints) = 100 cells: past the
    // 92-cell prose cap, so exactly two wrapped rows. Each jamo chain is one
    // cluster; a torn jamo sequence would break the count or drop glyphs.
    let syllable = "\u{1112}\u{1161}\u{11ab}"; // 한 decomposed
    let reply = syllable.repeat(50);
    let mut command = dalgon_command(dir.path(), &[&reply])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // The needle sits inside the first wrapped row.
    let recorder = prompt_and_remember(&mut terminal, "go", &syllable.repeat(20))?;
    let text = all_text(&recorder);
    let jamo_rows = text
        .lines()
        .filter(|line| line.contains('\u{1112}'))
        .count();
    assert_eq!(
        jamo_rows, 2,
        "a 100-cell decomposed reply must occupy exactly two wrapped rows: {text}"
    );
    assert!(
        !text.contains('\u{fffd}'),
        "decomposed jamo must never surface replacement glyphs: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn ime_style_jamo_backspace_removes_whole_syllable() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // IME-style input: each jamo arrives as its own write, as an input
    // method feeding codepoints would send them.
    for codepoint in ['\u{1112}', '\u{1161}', '\u{11ab}'] {
        terminal.write(codepoint.to_string().as_bytes())?;
        terminal.collect_for(Duration::from_millis(60))?;
    }
    terminal.write(b"ab")?;
    terminal.collect_for(SETTLE)?;
    // Three backspaces: 'b', 'a', then the whole 한 cluster as one unit —
    // never one jamo at a time.
    terminal.write(b"\x7f")?;
    terminal.write(b"\x7f")?;
    terminal.write(b"\x7f")?;
    terminal.collect_for(SETTLE)?;
    // Fresh text proves the buffer is truly empty.
    terminal.write(b"a")?;
    terminal.collect_for(SETTLE)?;
    let recorder = prompt_and_remember(&mut terminal, "", "done")?;
    let text = all_text(&recorder);
    assert!(
        text.lines().any(|line| line.trim_end() == "> a"),
        "the submitted prompt must be exactly \"a\": {text}"
    );
    assert!(
        !text.lines().any(|line| line.contains('\u{1112}')
            || line.contains('\u{1161}')
            || line.contains('\u{11ab}')),
        "no lone jamo may survive backspace: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn ambiguous_chars_widen_under_korean_locale() -> TestResult {
    let dir = TestDir::new()?;
    // '·' is EastAsianWidth=Ambiguous: one cell narrowly, two under ko_KR.
    // 60 of them fit one 92-cell row narrowly but need 120 cells — two rows —
    // when widened.
    let reply = "·".repeat(60);
    let needle = &reply[..40];
    let dotted_rows = |recorder: &VtRecorder| {
        all_text(recorder)
            .lines()
            .filter(|line| line.matches('·').count() > 10)
            .count()
    };
    let mut cjk = dalgon_command(dir.path(), &[&reply])?;
    cjk.env("LC_ALL", "ko_KR.UTF-8")
        .args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut cjk, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let recorder = prompt_and_remember(&mut terminal, "go", needle)?;
    let rows = dotted_rows(&recorder);
    assert_eq!(
        rows,
        2,
        "ko_KR locale must widen '·' onto a second row: {}",
        all_text(&recorder)
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn compatibility_jamo_replies_measure_two_cells() -> TestResult {
    let dir = TestDir::new()?;
    // Compatibility jamo ㄱ (U+3131) is a wide letter. 50 of them = 100
    // cells: past the prose cap, so exactly two rows, like any CJK text.
    let reply = "ㄱ".repeat(50);
    let mut command = dalgon_command(dir.path(), &[&reply])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let recorder = prompt_and_remember(&mut terminal, "go", &"ㄱ".repeat(20))?;
    let text = all_text(&recorder);
    let jamo_rows = text.lines().filter(|line| line.contains('ㄱ')).count();
    assert_eq!(
        jamo_rows, 2,
        "compatibility jamo must measure two cells and wrap twice: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn mixed_hangul_prompt_echoes_without_replacement() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // Precomposed + decomposed + halfwidth jamo in one prompt: the echoed
    // transcript must carry every codepoint intact.
    let prompt = "한\u{1112}\u{1161}\u{11ab}\u{ffa1}ok";
    let recorder = prompt_and_remember(&mut terminal, prompt, "done")?;
    let text = all_text(&recorder);
    assert!(
        text.contains(prompt),
        "the mixed Hangul prompt must echo verbatim: {text}"
    );
    assert!(
        !text.contains('\u{fffd}'),
        "no replacement glyph may appear: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}
