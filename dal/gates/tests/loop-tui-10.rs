#![cfg_attr(
    not(unix),
    expect(missing_docs, reason = "the whole crate is cfg'd out off unix")
)]
#![cfg(unix)]
#![expect(
    clippy::disallowed_methods,
    reason = "SC test runs the real dalgon binary"
)]
//! CJK and grapheme-cluster correctness on the real terminal grid: wide-cell
//! wrapping, ambiguous-width locale switching, mid-cluster backspace
//! boundaries, and split UTF-8 input. A split cluster, a half-glyph wrap, or
//! a locale that fails to widen ambiguous characters is a failure.

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
                    "dalgon did not exit cleanly after CJK input: {status}"
                );
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("dalgon stayed alive after repeated submit/Ctrl-D quit attempts".into())
}

/// Types `text`, submits it, and waits for the scripted reply, leaving the
/// settled frame available on the recorder.
fn prompt_and_remember(
    terminal: &mut PtyProcess,
    recorder: &mut VtRecorder,
    text: &str,
    reply: &str,
) -> TestResult {
    terminal.write(text.as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\r")?;
    terminal.wait_for(reply.as_bytes(), SPAWN)?;
    recorder.feed(terminal.output());
    Ok(())
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
fn cjk_reply_wraps_on_cell_boundary() -> TestResult {
    let dir = TestDir::new()?;
    // 50 full-width characters = 100 cells: more than the 92-cell prose cap,
    // so the reply must wrap whole clusters into a second row, never half a glyph.
    let reply = "漢".repeat(50);
    let mut command = dalgon_command(dir.path(), &[&reply])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let mut recorder = VtRecorder::new(100, 30);
    // The needle must fit inside one wrapped row: the reply splits into
    // 46 + 4 cells of characters, so wait only on its first 40 clusters.
    prompt_and_remember(&mut terminal, &mut recorder, "go", &"漢".repeat(40))?;
    let rendered = all_text(&recorder);
    assert_eq!(
        rendered.matches('漢').count(),
        50,
        "the 50-cluster reply must render exactly once: {rendered}"
    );
    let mut cjk_rows: Vec<&str> = rendered
        .lines()
        .filter(|line| line.contains('漢'))
        .collect();
    cjk_rows.sort_unstable();
    cjk_rows.dedup();
    assert_eq!(
        cjk_rows.len(),
        2,
        "a 100-cell CJK reply must occupy exactly two distinct wrapped rows: {rendered}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn ambiguous_chars_widen_under_cjk_locale() -> TestResult {
    let dir = TestDir::new()?;
    // '·' is EastAsianWidth=Ambiguous: one cell in a plain locale, two under
    // a CJK locale. 60 of them fit one 92-cell row narrowly but need 120
    // cells — two rows — when widened.
    let reply = "·".repeat(60);
    let needle = &reply[..40];

    // The footer hint row also carries '·' separators, so count only rows
    // that hold a long run of them.
    let dotted_rows = |recorder: &VtRecorder| {
        let mut rows: Vec<String> = all_text(recorder)
            .lines()
            .filter(|line| line.matches('·').count() > 10)
            .map(str::to_string)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows.len()
    };
    let mut narrow = dalgon_command(dir.path(), &[&reply])?;
    narrow.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut narrow, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let mut recorder = VtRecorder::new(100, 30);
    prompt_and_remember(&mut terminal, &mut recorder, "go", needle)?;
    let narrow_rows = dotted_rows(&recorder);
    assert_eq!(
        narrow_rows,
        1,
        "narrow locale must keep '·' on one row: {}",
        all_text(&recorder)
    );
    quit_cleanly(&mut terminal)?;

    let mut cjk = dalgon_command(dir.path(), &[&reply])?;
    cjk.env("LC_ALL", "ja_JP.UTF-8")
        .args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut cjk, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let mut recorder = VtRecorder::new(100, 30);
    prompt_and_remember(&mut terminal, &mut recorder, "go", needle)?;
    let cjk_rows = dotted_rows(&recorder);
    assert_eq!(
        cjk_rows,
        2,
        "CJK locale must widen '·' onto a second row: {}",
        all_text(&recorder)
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn backspace_removes_flag_cluster_whole() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // A flag is two regional indicators in one cluster: backspace must
    // delete both, not leave a lone indicator behind.
    terminal.write("ab🇯🇵".as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x7f")?;
    terminal.collect_for(SETTLE)?;
    let mut recorder = VtRecorder::new(100, 30);
    prompt_and_remember(&mut terminal, &mut recorder, "", "done")?;
    let text = all_text(&recorder);
    assert!(
        text.lines().any(|line| line.trim_end() == "> ab"),
        "the submitted prompt must be exactly \"ab\": {text}"
    );
    assert!(
        !text.lines().any(|line| line.contains('🇯')),
        "a lone regional indicator must never survive backspace: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn backspace_removes_zwj_and_combining_clusters_whole() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // Family emoji is one ZWJ-joined cluster; "e\u{301}" is a combining
    // cluster. Each backspace must remove the visible glyph, not one char.
    terminal.write("go👨‍👩‍👧".as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x7f")?;
    terminal.write("ne\u{301}".as_bytes())?;
    terminal.collect_for(SETTLE)?;
    terminal.write(b"\x7f")?;
    terminal.collect_for(SETTLE)?;
    let mut recorder = VtRecorder::new(100, 30);
    prompt_and_remember(&mut terminal, &mut recorder, "", "done")?;
    let text = all_text(&recorder);
    assert!(
        text.lines().any(|line| line.trim_end() == "> gon"),
        "both clusters must leave the composer whole: {text}"
    );
    assert!(
        !text
            .lines()
            .any(|line| line.contains('👨') || line.contains('\u{301}')),
        "no partial emoji or bare combining mark may remain: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn wide_cluster_at_text_margin_moves_whole() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // 80 columns give user text a 70-cell cap: 69 narrow chars + a 2-cell
    // 漢 overflow it, so 漢 must move whole onto the next rendered row.
    let mut input = "x".repeat(69);
    input.push('漢');
    let mut recorder = VtRecorder::new(80, 24);
    prompt_and_remember(&mut terminal, &mut recorder, &input, "done")?;
    let text = all_text(&recorder);
    assert!(
        text.lines().any(|line| line.trim_end() == "> 漢"),
        "the 2-cell cluster must move whole to the next row: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn flag_pair_measures_two_cells_at_wrap_boundary() -> TestResult {
    let dir = TestDir::new()?;
    // 80 columns give assistant prose a 72-cell cap. 68 + flag + "yz" is
    // exactly 72 when the flag measures 2; a 4-cell flag would push "yz"
    // onto a second row.
    let reply = format!("{}🇯🇵yz", "x".repeat(68));
    let mut command = dalgon_command(dir.path(), &[&reply])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 80, 24)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    let mut recorder = VtRecorder::new(80, 24);
    prompt_and_remember(&mut terminal, &mut recorder, "go", "🇯🇵yz")?;
    let text = all_text(&recorder);
    assert!(
        text.lines()
            .any(|line| line.contains(&format!("{}🇯🇵yz", "x".repeat(68)))),
        "the flag must occupy two cells so the whole reply fits one row: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn split_utf8_sequence_buffers_until_complete() -> TestResult {
    let dir = TestDir::new()?;
    let mut command = dalgon_command(dir.path(), &["done"])?;
    command.args(["--screen", "inline"]);
    let mut terminal = PtyProcess::spawn(&mut command, 100, 30)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    // A multibyte sequence split across PTY reads must reassemble into one
    // character, not surface as replacement glyphs.
    terminal.write(b"a")?;
    for byte in [0xe6_u8, 0x97, 0xa5] {
        terminal.write(&[byte])?;
        terminal.collect_for(Duration::from_millis(60))?;
    }
    terminal.write(b"b")?;
    terminal.collect_for(SETTLE)?;
    let mut recorder = VtRecorder::new(100, 30);
    prompt_and_remember(&mut terminal, &mut recorder, "", "done")?;
    let text = all_text(&recorder);
    assert!(
        text.lines().any(|line| line.trim_end() == "> a日b"),
        "the split sequence must submit as a single 日: {text}"
    );
    assert!(
        !text.contains('\u{fffd}'),
        "no replacement character may appear: {text}"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}

#[test]
fn cjk_cwd_basename_renders_in_status() -> TestResult {
    let dir = TestDir::new()?;
    let workspace = dir.path().join("漢字ワーク");
    std::fs::create_dir_all(&workspace)?;
    let mut command = dalgon_command(dir.path(), &["unused"])?;
    command.args(["--screen", "inline"]).current_dir(&workspace);
    // Below 60 columns the status segment switches to the path basename.
    let mut terminal = PtyProcess::spawn(&mut command, 44, 24)?;
    terminal.wait_for(dal_tui::copy::ids::COMPOSER_PLACEHOLDER.as_bytes(), SPAWN)?;
    terminal.collect_for(SETTLE)?;
    let mut recorder = VtRecorder::new(44, 24);
    recorder.feed(terminal.output());
    assert!(
        all_text(&recorder).contains("漢字ワーク"),
        "the status line must render the CJK workspace basename"
    );
    quit_cleanly(&mut terminal)?;
    Ok(())
}
