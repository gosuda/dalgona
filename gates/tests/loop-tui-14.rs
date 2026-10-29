#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! Wave-14 width probes: a bare ZWJ, a VS16 on a non-emoji base, and a lone
//! conjoining mark must not claim two cells. `cluster_width` forces two
//! cells for any cluster holding ZWJ/VS16/keycap/jamo; that is right for
//! real emoji sequences (family glyphs, keycap digits, flags) but wrong
//! for `a\u{200d}`, `x\u{fe0f}`, or a lone `\u{200d}` — the terminal draws
//! those in one or zero cells, so a two-cell claim wraps the transcript
//! early, strands a blank cell, and desyncs the composer cursor.

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

use std::{
    error::Error,
    time::{Duration, Instant},
};

use pty::vt::VtRecorder;
use pty::{PtyProcess, dalgon_command};
use support::TestDir;

const SPAWN: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(400);

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn quit_cleanly(terminal: &mut PtyProcess) -> TestResult {
    terminal.write(b"\r")?;
    terminal.collect_for(SETTLE)?;
    for _ in 0..4 {
        terminal.write(b"\x04")?;
        match terminal.wait_for_exit(Duration::from_secs(5)) {
            Ok(status) => {
                assert!(
                    status.success(),
                    "dalgon did not exit cleanly after width probes: {status}"
                );
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("dalgon stayed alive after repeated submit/Ctrl-D quit attempts".into())
}

fn prompt_and_remember(
    terminal: &mut PtyProcess,
    text: &str,
    reply: &str,
    columns: u16,
    rows: u16,
) -> Result<VtRecorder, Box<dyn Error + Send + Sync>> {
    terminal.write(text.as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\r")?;
    terminal.wait_for(reply.as_bytes(), SPAWN)?;
    let deadline = Instant::now() + SPAWN;
    let mut last_len = 0usize;
    let mut stable = 0usize;
    loop {
        terminal.collect_for(Duration::from_millis(60))?;
        let output = terminal.output();
        let mut probe = VtRecorder::new(columns, rows);
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
            let cause = if busy {
                "busy status still visible".to_string()
            } else if stable == 0 {
                "output still arriving".to_string()
            } else {
                format!("quiet-and-idle for only {stable} of 3 required polls")
            };
            return Err(
                format!("turn frame never settled after the reply arrived: {cause}").into(),
            );
        }
    }
}

fn all_text(recorder: &VtRecorder) -> String {
    recorder
        .screen_rows()
        .into_iter()
        .chain(recorder.scrollback_rows())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rows holding at least `min` copies of `ch`, deduped. Mirrors the
/// `ambiguous_chars_widen_under_cjk_locale` counting pattern.
fn rows_with_runs(recorder: &VtRecorder, ch: char, min: usize) -> usize {
    let mut rows: Vec<String> = all_text(recorder)
        .lines()
        .filter(|line| line.matches(ch).count() >= min)
        .map(str::to_string)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows.len()
}

/// Drives two scripted replies through one session: `baseline` uses only
/// one-cell glyphs and `probe` swaps its run character for the probe
/// cluster. Row counts are compared — never a hardcoded wrap budget —
/// so the assertion survives prose-width changes.
fn probe_rows_vs_baseline(
    baseline_ch: char,
    probe: &str,
    marker: char,
) -> Result<usize, Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let baseline = format!("{} BASE", baseline_ch.to_string().repeat(60));
    let probe = format!("{probe} END");
    let mut command = dalgon_command(dir.path(), &[&baseline, &probe])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // Prompt twice: first turn renders the baseline reply, second the probe.
    let _ = prompt_and_remember(&mut terminal, "go", "BASE", 100, 30)?;
    let recorder = prompt_and_remember(&mut terminal, "go", "END", 100, 30)?;
    let base_rows = rows_with_runs(&recorder, baseline_ch, 10);
    let probe_rows = rows_with_runs(&recorder, marker, 10);
    assert!(
        base_rows >= 1,
        "baseline reply of 60 one-cell glyphs never rendered: {}",
        all_text(&recorder)
    );
    quit_cleanly(&mut terminal)?;
    let baseline_rows = base_rows;
    if baseline_rows != probe_rows {
        return Err(format!(
            "probe rendered {probe_rows} rows vs baseline {baseline_rows}: {}",
            all_text(&recorder)
        )
        .into());
    }
    Ok(probe_rows)
}

#[test]
fn ascii_zwj_tail_is_one_cell_not_two() -> TestResult {
    // `a\u{200d}`: the joiner is zero-width, so the cluster stays one cell.
    // A defect that claims two cells per joiner wraps the reply earlier and
    // burns more rows than the same glyph count of plain 'a's.
    probe_rows_vs_baseline('a', &"a\u{200d}".repeat(60), 'a')?;
    Ok(())
}

#[test]
fn variation_selector_on_ascii_is_one_cell_not_two() -> TestResult {
    // `x\u{fe0f}`: VS16 requests emoji presentation on a base that has none,
    // so terminals ignore it — one cell, not two.
    probe_rows_vs_baseline('b', &"x\u{fe0f}".repeat(60), 'x')?;
    Ok(())
}

#[test]
fn lone_zwj_is_zero_cells_not_two() -> TestResult {
    let dir = TestDir::new()?;
    // "S" + 50 bare ZWJ + "E": 2 real cells. If each ZWJ claims two cells
    // the reply wraps at the prose cap and "SE" never sits on one row.
    let reply = format!("S{}E", "\u{200d}".repeat(50));
    let mut command = dalgon_command(dir.path(), &[&reply])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let recorder = prompt_and_remember(&mut terminal, "go", "E", 100, 30)?;
    let text = all_text(&recorder);
    let joined = text
        .lines()
        .any(|line| line.replace('\u{200d}', "").contains("SE"));
    assert!(
        joined,
        "a lone ZWJ is zero cells: 'S' and 'E' must stay on one row: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

/// Drives one composer turn with reply bytes injected after the startup
/// probe window, then returns the rendered screen text. The composer must
/// hold exactly `typed` afterwards.
fn composer_after_reply_bytes(
    typed: &str,
    injected: &[u8],
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["DONE"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(typed.as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(injected)?;
    terminal.collect_for(SETTLE)?;
    let mut probe = VtRecorder::new(100, 30);
    probe.feed(terminal.output());
    let text = all_text(&probe);
    quit_cleanly(&mut terminal)?;
    Ok(text)
}

#[test]
fn late_terminal_replies_do_not_type_into_composer() -> TestResult {
    // Terminal replies that land after the probe window — an OSC 11
    // background reply, a kitty-graphics APC, a C1-form DSR, and a C1
    // string — must be consumed, never typed: the composer stays "ok".
    let injected: &[u8] =
        b"\x1b]11;rgb:1111/2222/3333\x07\x1b_Gi=31;OK\x1b\\\x9b13;1R\x90Payload\x9c";
    let text = composer_after_reply_bytes("ok", injected)?;
    for leak in ["rgb", "Gi=31", "13;1R", "Payload"] {
        assert!(
            !text.contains(leak),
            "reply payload {leak} leaked into the composer: {text}"
        );
    }
    assert!(
        text.contains("ok"),
        "typed text missing from screen: {text}"
    );
    Ok(())
}

#[test]
fn drip_fed_reply_string_does_not_type_into_composer() -> TestResult {
    // The same OSC reply split across two reads inside the escape window
    // must still be consumed end to end.
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["DONE"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.write(b"ok")?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x1b]11;rgb:1")?;
    terminal.collect_for(Duration::from_millis(20))?;
    terminal.write(b"111/2222/3333\x07")?;
    terminal.collect_for(SETTLE)?;
    let mut probe = VtRecorder::new(100, 30);
    probe.feed(terminal.output());
    let text = all_text(&probe);
    assert!(
        !text.contains("rgb") && !text.contains("1111"),
        "split OSC reply leaked into the composer: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn real_emoji_sequences_stay_two_cells() -> TestResult {
    let dir = TestDir::new()?;
    // Guard against over-correction: real ZWJ emoji still occupy two cells,
    // so 60 of them must wrap further than 60 one-cell glyphs.
    let baseline = format!("{} BASE", "a".repeat(60));
    let probe = format!("{} END", "👨‍👩‍👧".repeat(60));
    let mut command = dalgon_command(dir.path(), &[&baseline, &probe])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let _ = prompt_and_remember(&mut terminal, "go", "BASE", 100, 30)?;
    let recorder = prompt_and_remember(&mut terminal, "go", "END", 100, 30)?;
    let base_rows = rows_with_runs(&recorder, 'a', 10);
    let emoji_rows = rows_with_runs(&recorder, '👨', 10);
    assert!(
        emoji_rows > base_rows,
        "60 two-cell emoji must wrap further than 60 one-cell glyphs          (base {base_rows}, emoji {emoji_rows}): {}",
        all_text(&recorder)
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}
