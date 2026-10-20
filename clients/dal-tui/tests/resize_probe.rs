//! Resize probe: scripted width-cycle loop evidence.

#![cfg(test)]

mod common;

use common::{
    Action, COMMITTED, Chunk, Frame, Gate, Log, PROBE_REPLIES, ProbeAgent, ProbeHost, ProbeIo,
    SERIAL, probe_options,
};
use dal_tui::screen::inline::{Region, commit_bytes, shrink_bytes};
use dal_tui::term::CursorReport;
use dal_tui::transcript::Transcript;

fn committed_strings() -> Vec<String> {
    COMMITTED.iter().map(|row| (*row).to_owned()).collect()
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn count_token(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Parses one `ESC[<row>;<col>H` / `ESC[<row>H` target; `None` when the
/// bytes at `index` open no cursor-positioning sequence.
fn parse_cup_at(bytes: &[u8], index: usize) -> Option<(u16, usize)> {
    if bytes.get(index) != Some(&0x1b) || bytes.get(index + 1) != Some(&b'[') {
        return None;
    }
    let mut end = index + 2;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == index + 2 {
        return None;
    }
    let row = std::str::from_utf8(bytes.get(index + 2..end)?)
        .ok()?
        .parse::<u16>()
        .ok()?;
    if bytes.get(end) == Some(&b'H') {
        return Some((row, end + 1));
    }
    if bytes.get(end) != Some(&b';') {
        return None;
    }
    let mut col_end = end + 1;
    while bytes.get(col_end).is_some_and(u8::is_ascii_digit) {
        col_end += 1;
    }
    if bytes.get(col_end) != Some(&b'H') {
        return None;
    }
    Some((row, col_end + 1))
}

/// Row numbers referenced by `ESC[<row>;<col>H` / `ESC[<row>H` sequences.
fn targeted_rows(bytes: &[u8]) -> Vec<u16> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index + 2 <= bytes.len() {
        let Some((row, next)) = parse_cup_at(bytes, index) else {
            index += 1;
            continue;
        };
        rows.push(row);
        index = next;
    }
    rows
}

fn assert_no_scrollback_clear(bytes: &[u8], label: &str) {
    assert!(
        !has(bytes, b"\x1b[J"),
        "{label}: erase-display ED0 present: {bytes:?}"
    );
    assert!(
        !has(bytes, b"\x1b[2J"),
        "{label}: erase-display ED2 present: {bytes:?}"
    );
    assert!(
        !has(bytes, b"\x1b[3J"),
        "{label}: scrollback clear present: {bytes:?}"
    );
}

fn assert_one_bracket_pair(bytes: &[u8], label: &str) {
    let open = count_token(bytes, b"\x1b[?2026h");
    let close = count_token(bytes, b"\x1b[?2026l");
    assert_eq!(
        open, 1,
        "{label}: expected exactly one sync open, got {open}"
    );
    assert_eq!(
        close, 1,
        "{label}: expected exactly one sync close, got {close}"
    );
}

/// Byte offsets of every committed row inside `bytes`, in order.
fn committed_byte_offsets(bytes: &[u8], texts: &[String]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut cursor = 0;
    for text in texts {
        let found = bytes[cursor..]
            .windows(text.len())
            .position(|w| w == text.as_bytes())
            .unwrap_or_else(|| panic!("committed row {text:?} missing from frame bytes"));
        offsets.push(cursor + found);
        cursor += found + text.len();
    }
    offsets
}

#[test]
fn shrink_80_to_40_preserves_committed_rows_with_zero_repaints_above_live_top() {
    let texts = committed_strings();
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("resize-probe-0", &texts), texts);
    assert_eq!(transcript.rows(), texts.as_slice());

    // One live block directly under the 3 committed rows: commit top 6.
    let live = Region { top: 9, height: 6 };
    let commit = commit_bytes(&texts, 6, true);
    assert_one_bracket_pair(&commit, "commit@80");
    assert_no_scrollback_clear(&commit, "commit@80");
    let offsets = committed_byte_offsets(&commit, &texts);
    assert!(
        offsets.windows(2).all(|w| w[0] < w[1]),
        "committed rows land once, in order: {offsets:?}"
    );
    for row in targeted_rows(&commit) {
        assert!(
            row >= 6,
            "commit write to row {row} above commit top 6: {:?}",
            targeted_rows(&commit)
        );
    }

    // Shrink 80 -> 40: the settled transcript is width-independent.
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "shrink must preserve rows"
    );
    let surplus = 2_u16;
    let first_surplus = live.top + live.height - surplus;
    let shrink = shrink_bytes(first_surplus, surplus, true);
    assert_one_bracket_pair(&shrink, "shrink@40");
    assert_no_scrollback_clear(&shrink, "shrink@40");
    assert_eq!(
        count_token(&shrink, b"K"),
        usize::from(surplus),
        "shrink erases exactly the surplus rows"
    );
    for row in targeted_rows(&shrink) {
        assert!(
            row >= live.top,
            "shrink write to row {row} above live top {}: {:?}",
            live.top,
            targeted_rows(&shrink)
        );
    }
    for text in &texts {
        assert!(
            !has(&shrink, text.as_bytes()),
            "shrink repainted committed row {text:?}"
        );
    }
    // No overlay traffic inside the byte driver.
    assert!(!has(&shrink, b"\x1b[?1049h") && !has(&shrink, b"\x1b[?1049l"));
}

#[test]
fn shrink_cycle_recommit_emits_nothing_and_repeats_no_committed_bytes() {
    // A retry that re-commits settled entries after a shrink must emit
    // nothing: otherwise the transcript would hold the rows twice and a
    // later frame could duplicate the live block's settled bytes.
    let texts = committed_strings();
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("resize-probe-0", &texts), texts);

    let live = Region { top: 9, height: 6 };
    let commit = commit_bytes(&texts, 6, true);
    assert_one_bracket_pair(&commit, "commit@80");
    assert_no_scrollback_clear(&commit, "commit@80");

    // Shrink 80 -> 40 erases the two surplus live rows in place.
    let surplus = 2_u16;
    let shrink = shrink_bytes(live.top + live.height - surplus, surplus, true);
    assert_one_bracket_pair(&shrink, "shrink@40");
    assert_no_scrollback_clear(&shrink, "shrink@40");

    // The settled entries are width-independent: a re-commit after the
    // shrink emits no rows and changes no transcript state.
    assert!(
        transcript.commit("resize-probe-0", &texts).is_empty(),
        "re-commit after shrink must emit nothing"
    );
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "shrink must preserve rows"
    );

    // Across the whole cycle each committed row occurs exactly once: the
    // commit frame owns the bytes, the shrink frame repeats none of them.
    let cycle: Vec<u8> = commit.iter().chain(shrink.iter()).copied().collect();
    for text in &texts {
        assert_eq!(
            count_token(&cycle, text.as_bytes()),
            1,
            "committed row {text:?} duplicated across the shrink cycle"
        );
    }
}

#[test]
fn grow_40_to_100_preserves_committed_rows_with_zero_repaints_above_live_top() {
    let texts = committed_strings();
    let mut transcript = Transcript::default();
    assert_eq!(transcript.commit("resize-probe-0", &texts), texts);

    // Grow 40 -> 100: rows are identical; the live block keeps one Region.
    assert_eq!(
        transcript.rows(),
        texts.as_slice(),
        "grow must preserve rows"
    );
    let commit = commit_bytes(&texts, 6, true);
    let offsets = committed_byte_offsets(&commit, &texts);
    assert!(
        offsets.windows(2).all(|w| w[0] < w[1]),
        "grow keeps commit order: {offsets:?}"
    );
    // A grow repaints the relocated live block in place; model it as a
    // shrink of zero surplus plus a live repaint: every targeted row must
    // still sit at or below the single live top.
    for row in targeted_rows(&commit) {
        assert!(
            row >= 6,
            "grow write to row {row} above live top 6: {:?}",
            targeted_rows(&commit)
        );
    }
    assert_no_scrollback_clear(&commit, "grow@100");
    assert!(!has(&commit, b"\x1b[?1049h") && !has(&commit, b"\x1b[?1049l"));
}

#[test]
fn exactly_one_live_block_per_frame_write() {
    let texts = committed_strings();
    // A Region is one (top, height) pair by construction: the driver owns a
    // single live block, never two.
    let live = Region { top: 12, height: 5 };
    let commit = commit_bytes(&texts, 9, true);
    assert_one_bracket_pair(&commit, "one-block commit");
    let shrink = shrink_bytes(live.top + live.height - 2, 2, true);
    assert_one_bracket_pair(&shrink, "one-block shrink");
    // All targeted rows of one frame lie in a single contiguous interval
    // starting at the live top: no second block above or below.
    for (label, bytes, floor) in [("commit", commit, 9_u16), ("shrink", shrink, live.top)] {
        let mut rows = targeted_rows(&bytes);
        rows.sort_unstable();
        rows.dedup();
        assert!(!rows.is_empty(), "{label}: frame wrote no rows");
        assert!(
            rows.iter().all(|row| *row >= floor),
            "{label}: rows {rows:?} escape the single block at {floor}"
        );
        let first = rows.first().expect("frame wrote rows");
        let last = rows.last().expect("frame wrote rows");
        let span = usize::from(last.saturating_sub(*first).saturating_add(1));
        assert!(
            span <= rows.len() + 4,
            "{label}: rows {rows:?} are not one contiguous block"
        );
    }
}

#[test]
fn byte_driver_never_touches_overlay_or_scrollback_clear() {
    let texts = committed_strings();
    for (label, bytes) in [
        ("commit", commit_bytes(&texts, 6, true)),
        ("shrink", shrink_bytes(13, 2, true)),
        ("commit-nosync", commit_bytes(&texts, 6, false)),
        ("shrink-nosync", shrink_bytes(13, 2, false)),
    ] {
        assert_no_scrollback_clear(&bytes, label);
        assert!(
            !has(&bytes, b"\x1b[?1049h") && !has(&bytes, b"\x1b[?1049l"),
            "{label}: byte driver must not enter the alternate screen"
        );
    }
}

struct Report {
    events: Vec<String>,
    frames: Vec<Frame>,
    resize_marks: Vec<(usize, u16)>,
}

fn drive() -> Report {
    let _serial = SERIAL.lock().expect("probe serial");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    let log = Log::default();
    let host = ProbeHost {
        agent: ProbeAgent { log: log.clone() },
    };
    let opts = probe_options(runtime.handle());
    let script = PROBE_REPLIES
        .iter()
        .map(|reply| Chunk {
            gate: Gate::Open,
            action: Action::Input((*reply).to_vec()),
        })
        .chain([
            Chunk {
                gate: Gate::Frames(2),
                action: Action::Resize(40, 24, CursorReport::Position(20, 1)),
            },
            Chunk {
                gate: Gate::Frames(4),
                action: Action::Resize(100, 24, CursorReport::Position(18, 1)),
            },
            Chunk {
                gate: Gate::Frames(6),
                action: Action::Input(b"\x04".to_vec()),
            },
        ])
        .collect::<Vec<_>>();
    let io = ProbeIo::new(script, log);
    dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()))
        .expect("the resize probe loop exits cleanly");
    let mut state = io.state.lock().expect("probe state");
    Report {
        events: io.log.events(),
        frames: std::mem::take(&mut state.frames),
        resize_marks: std::mem::take(&mut state.resize_marks),
    }
}

#[test]
fn loop_shrink_then_grow_never_clears_screen_and_quits_cleanly() {
    let report = drive();
    assert!(
        !report.frames.is_empty(),
        "the loop wrote no frame bytes at all"
    );
    // Committed rows reach the terminal exactly via the commit path.
    let commit_hits = COMMITTED
        .iter()
        .map(|needle| {
            report
                .frames
                .iter()
                .filter(|f| has(&f.bytes, needle.as_bytes()))
                .count()
        })
        .collect::<Vec<_>>();
    for (needle, hits) in COMMITTED.iter().zip(commit_hits.iter()) {
        assert_eq!(
            *hits, 1,
            "committed row {needle:?} must be written exactly once (commit-only): got {hits}"
        );
    }
    // No frame ever clears the display or the scrollback: committed rows
    // survive every resize by construction.
    for (index, frame) in report.frames.iter().enumerate() {
        assert_no_scrollback_clear(&frame.bytes, &format!("loop frame {index}"));
    }
    // Every resize frame respects its own measured top: byte-indexed proof
    // of zero repaints above the live block, windowed per resize epoch so a
    // later grow to a lower top cannot retroactively fail an earlier shrink.
    assert_eq!(
        report.resize_marks.len(),
        2,
        "shrink + grow must each measure once: {:?}",
        report.resize_marks
    );
    let total = report.frames.len();
    for (mark, window) in report
        .resize_marks
        .windows(2)
        .map(|w| (w[0], w[1].0))
        .chain(report.resize_marks.last().map(|last| (*last, total)))
    {
        let (mark_index, top) = mark;
        for (index, frame) in report.frames.iter().enumerate() {
            if index < mark_index || index >= window {
                continue;
            }
            for row in targeted_rows(&frame.bytes) {
                assert!(
                    row >= top,
                    "frame {index} writes row {row} above measured top {top}"
                );
            }
        }
    }
    // Exactly one live block: at most one sync bracket pair per frame.
    for (index, frame) in report.frames.iter().enumerate() {
        let open = count_token(&frame.bytes, b"\x1b[?2026h");
        let close = count_token(&frame.bytes, b"\x1b[?2026l");
        assert!(
            open <= 1 && close <= 1,
            "loop frame {index}: one live block max, got {open} opens {close} closes"
        );
    }
    // Quit transition: Ctrl+D leaves the loop and closes the host session.
    assert!(
        report.events.iter().any(|e| e == "host:close"),
        "quit did not close the host session: {:?}",
        report.events
    );
}
