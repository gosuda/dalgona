//! Resize-review R5 behavioral matrix.
//!
//! This file reuses the shared scripted terminal harness in `common`
//! for width-cycle evidence; every assertion observes the bytes emitted
//! by `run_backend`.

#![cfg(test)]

mod common;

use common::{
    Action, Chunk, Gate, Log, PROBE_REPLIES, ProbeAgent, ProbeHost, ProbeIo, SERIAL, probe_options,
};
use dal_core::{Command, ModelRoute};
use dal_tui::picker::ModelOption;
use dal_tui::term::CursorReport;

/// Actions exposed to the matrix without exposing the probe's private
/// implementation types to the integration-test parent module.
enum MatrixAction {
    Input(&'static [u8]),
    Resize(u16, u16, CursorReport),
}

struct MatrixReport {
    frames: Vec<Vec<u8>>,
    events: Vec<String>,
    resize_marks: Vec<(usize, u16)>,
}

fn matrix_drive(actions: Vec<(usize, MatrixAction)>) -> MatrixReport {
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
        .chain(actions.into_iter().map(|(gate, action)| Chunk {
            gate: Gate::Frames(gate),
            action: match action {
                MatrixAction::Input(bytes) => Action::Input(bytes.to_vec()),
                MatrixAction::Resize(width, height, report) => {
                    Action::Resize(width, height, report)
                }
            },
        }))
        .collect();
    let io = ProbeIo::new(script, log);
    dal_tui::run_backend(&host, &opts, &io, || {
        Ok(vec![
            ModelOption {
                label: "alpha · t-alpha".into(),
                command: Command::SetModel {
                    model: ModelRoute::Synthetic {
                        id: "t-alpha".into(),
                    },
                    save: dal_core::command::Save::SessionAndDefault,
                },
                scoped_id: None,
            },
            ModelOption {
                label: "beta · t-beta".into(),
                command: Command::SetModel {
                    model: ModelRoute::Synthetic {
                        id: "t-beta".into(),
                    },
                    save: dal_core::command::Save::SessionAndDefault,
                },
                scoped_id: None,
            },
        ])
    })
    .expect("resize matrix loop exits cleanly");
    let mut state = io.state.lock().expect("probe state");
    MatrixReport {
        frames: state.frames.drain(..).map(|frame| frame.bytes).collect(),
        events: io.log.events(),
        resize_marks: state.resize_marks.clone(),
    }
}

const COMMITTED: [&[u8]; 3] = [
    b"resize-probe committed alpha 01",
    b"resize-probe committed beta 02",
    b"resize-probe committed gamma 03",
];

fn has(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

fn count(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The last frame the live loop painted: paints carry the sync-update bracket
/// the probe reply enables, while the cursor-park and exit-line writes that
/// end the recording carry none.
fn last_live_frame(report: &MatrixReport) -> &[u8] {
    report
        .frames
        .iter()
        .rfind(|frame| has(frame, b"\x1b[?2026h"))
        .expect("the loop painted a live frame")
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

/// Every cursor-positioning sequence in paint order: row, start, end offsets.
fn cups(bytes: &[u8]) -> Vec<(u16, usize, usize)> {
    let mut out = Vec::new();
    let mut index = 0;
    while index + 2 <= bytes.len() {
        let Some((row, next)) = parse_cup_at(bytes, index) else {
            index += 1;
            continue;
        };
        out.push((row, index, next));
        index = next;
    }
    out
}

/// Row numbers referenced by `ESC[<row>;<col>H` / `ESC[<row>H` sequences.
fn targeted_rows(bytes: &[u8]) -> Vec<u16> {
    cups(bytes).into_iter().map(|(row, _, _)| row).collect()
}

/// Painted live rows of one frame: each addressed row whose write carries
/// bytes beyond the erase, in paint order with its payload. Positions stay
/// absolute; callers check origins per run and payloads across runs. Blank
/// rows carry no bytes and are not listed.
fn live_block(frame: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let positioned = cups(frame);
    let mut rows = Vec::new();
    for (i, &(row, _, end)) in positioned.iter().enumerate() {
        let stop = positioned
            .get(i + 1)
            .map_or(frame.len(), |&(_, start, _)| start);
        let mut payload = &frame[end..stop];
        if let Some(stripped) = payload.strip_prefix(b"\x1b[2K") {
            payload = stripped;
        }
        while payload.len() >= 3 && payload[0] == 0x1b && payload[1] == b'[' {
            let mut sgr = 2;
            while sgr < payload.len() && (payload[sgr].is_ascii_digit() || payload[sgr] == b';') {
                sgr += 1;
            }
            if payload.get(sgr) != Some(&b'm') {
                break;
            }
            payload = &payload[sgr + 1..];
        }
        if !payload.is_empty() && payload[0] != 0x1b {
            rows.push((row, payload.to_vec()));
        }
    }
    rows
}

/// Checks one converged live frame against the inline anchor rule: contiguous
/// rows inside the screen from the rule-assigned origin, hidden caret parked
/// at the block top. `top` is the last measured top; `prev_h`/`new_h` are the
/// painted heights around the last resize, which bottom-anchors on change.
fn assert_live_block(live: &[u8], top: u16, prev_h: u16, new_h: u16) {
    let block = live_block(live);
    assert!(!block.is_empty(), "the live frame painted no live rows");
    let rows: Vec<u16> = block.iter().map(|(row, _)| *row).collect();
    assert!(
        rows.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "live rows are not one contiguous block: {rows:?}"
    );
    let origin = if prev_h == new_h {
        top
    } else {
        let bottom = new_h
            .saturating_sub(u16::try_from(block.len()).expect("small live block"))
            .saturating_add(1);
        top.max(bottom)
    };
    assert_eq!(
        rows[0], origin,
        "live origin breaks the anchor rule for top {top}, heights {prev_h}->{new_h}"
    );
    assert!(
        *rows.last().expect("live block is not empty") <= new_h,
        "live rows escape the screen: {rows:?}"
    );
    let caret = targeted_rows(live)
        .last()
        .copied()
        .expect("the caret parked somewhere");
    assert_eq!(
        caret, rows[0],
        "caret not parked at the block top: caret {caret}, block {rows:?}"
    );
}

fn assert_safe_frames(report: &MatrixReport) {
    assert!(!report.frames.is_empty(), "run_backend emitted no frames");
    for (index, bytes) in report.frames.iter().enumerate() {
        assert!(
            !has(bytes, b"\x1b[J"),
            "frame {index} emitted ED0: {bytes:?}"
        );
        assert!(
            !has(bytes, b"\x1b[2J"),
            "frame {index} emitted ED2: {bytes:?}"
        );
        assert!(
            !has(bytes, b"\x1b[3J"),
            "frame {index} cleared scrollback: {bytes:?}"
        );
        assert!(
            count(bytes, b"\x1b[?2026h") <= 1 && count(bytes, b"\x1b[?2026l") <= 1,
            "frame {index} contains more than one live block: {bytes:?}"
        );
    }
}

fn assert_committed_once(report: &MatrixReport) {
    for row in COMMITTED {
        let occurrences = report.frames.iter().filter(|frame| has(frame, row)).count();
        assert_eq!(
            occurrences, 1,
            "committed row {row:?} was repainted {occurrences} times"
        );
    }
}

#[test]
fn cjk_and_emoji_draft_survives_shrink_and_grow_without_committed_repaint() {
    let report = matrix_drive(vec![
        (2, MatrixAction::Input("日本語🙂".as_bytes())),
        (
            3,
            MatrixAction::Resize(10, 24, CursorReport::Position(20, 1)),
        ),
        (
            4,
            MatrixAction::Resize(80, 24, CursorReport::Position(18, 1)),
        ),
        // Ctrl+D quits only on an empty composer (DESIGN.md: "Quit (composer
        // empty)"); over a draft it deletes forward and the loop keeps running.
        // One Backspace removes one grapheme, so four clear the four clusters
        // typed above, and only then does Ctrl+D leave the loop. Gate 0 is no
        // frame wait: the loop paints once per pass, so the frame for the
        // resize above is written before either chunk is read.
        (0, MatrixAction::Input(b"\x7f\x7f\x7f\x7f")),
        (0, MatrixAction::Input(b"\x04")),
    ]);
    assert_safe_frames(&report);
    assert_committed_once(&report);
    let draft = "日本語🙂".as_bytes();
    assert!(
        report.frames.iter().any(|frame| has(frame, draft)),
        "wide-cell draft bytes never reached a frame"
    );
    assert!(
        report.events.iter().any(|event| event == "host:close"),
        "matrix did not complete its quit transition"
    );
}

#[test]
fn floor_frames_paint_warning_while_picker_is_open() {
    let report = matrix_drive(vec![
        (2, MatrixAction::Input(b"/model\r")),
        (
            3,
            MatrixAction::Resize(11, 24, CursorReport::Position(20, 1)),
        ),
        (4, MatrixAction::Input(b"\x1b[B")),
        (5, MatrixAction::Resize(80, 7, CursorReport::Position(6, 1))),
        (
            6,
            MatrixAction::Resize(80, 24, CursorReport::Position(18, 1)),
        ),
        // Gate 0, as in the CJK scenario, is no frame wait; the harness holds
        // the Ctrl+D back until the Esc has closed the picker. An absolute
        // frame count here only restates how many frames the painter writes
        // for the steps above, and a count that is never reached leaves the
        // picker open, where Ctrl+D is inert.
        (0, MatrixAction::Input(b"\x1b")),
        (0, MatrixAction::Input(b"\x04")),
    ]);
    assert_safe_frames(&report);
    assert!(
        report.frames.iter().any(|frame| has(frame, b"dalgon need")),
        "the <12-column floor never painted its warning surface"
    );
    assert!(
        report
            .frames
            .iter()
            .any(|frame| has(frame, b"dalgon needs at least 8 rows")),
        "the <8-row floor never painted its warning surface"
    );
    assert!(
        report.frames.iter().any(|frame| has(frame, b"> alpha"))
            && !report.frames.iter().any(|frame| has(frame, b"> beta")),
        "picker input crossed the floor gate and changed selection"
    );
}

#[test]
fn picker_resize_keeps_one_live_block_and_preserves_committed_rows() {
    let report = matrix_drive(vec![
        (2, MatrixAction::Input(b"/model\r")),
        (
            3,
            MatrixAction::Resize(40, 12, CursorReport::Position(10, 1)),
        ),
        (0, MatrixAction::Input(b"\x1b")),
        (0, MatrixAction::Input(b"\x04")),
    ]);
    assert_safe_frames(&report);
    assert_committed_once(&report);
    assert!(
        report
            .frames
            .iter()
            .any(|frame| has(frame, b"Pick a model")),
        "picker was not opened through the real backend"
    );
}

#[test]
fn silent_dsr_on_overlay_exit_has_bounded_hold() {
    let report = matrix_drive(vec![
        (2, MatrixAction::Input(b"/model\r")),
        (3, MatrixAction::Resize(40, 12, CursorReport::Silent)),
        (0, MatrixAction::Input(b"\x1b")),
        (0, MatrixAction::Input(b"\x04")),
    ]);
    assert_safe_frames(&report);
    assert!(
        report
            .frames
            .iter()
            .any(|frame| has(frame, b"Pick a model")),
        "silent DSR scenario never opened the picker overlay"
    );
    assert!(
        report.frames.len() < 64,
        "silent DSR caused an unbounded blank/paint loop"
    );
    assert!(
        report.events.iter().any(|event| event == "host:close"),
        "silent DSR path never returned to a closable backend"
    );
}

#[test]
fn resize_storm_converges_to_last_size_without_repainting_committed_rows() {
    let report = matrix_drive(vec![
        // Equal frame gates intentionally queue all three size changes before
        // the next rendered frame, exercising the same-tick resize storm.
        (
            2,
            MatrixAction::Resize(60, 24, CursorReport::Position(20, 1)),
        ),
        (
            2,
            MatrixAction::Resize(40, 20, CursorReport::Position(16, 1)),
        ),
        (
            2,
            MatrixAction::Resize(100, 24, CursorReport::Position(18, 1)),
        ),
        (6, MatrixAction::Input(b"\x04")),
    ]);
    assert_safe_frames(&report);
    assert_committed_once(&report);
    // The recording ends with the cursor-park and exit-line writes, which are
    // not paints: geometry is read from the last live frame, where no
    // committed row may appear again and no row may sit above the last
    // measured top.
    let live = last_live_frame(&report);
    for row in COMMITTED {
        assert!(
            !has(live, row),
            "committed row {row:?} repainted in the final live frame"
        );
    }
    let (_, top) = report
        .resize_marks
        .last()
        .copied()
        .expect("each storm resize measured once");
    for row in targeted_rows(live) {
        assert!(
            row >= top,
            "final live frame writes row {row} above measured top {top}"
        );
    }
    // The storm's last resize changed height, the lone resize did not, so the
    // anchor rule assigns different origins to identical live text.
    assert_live_block(live, top, 20, 24);
    let baseline = matrix_drive(vec![
        (
            2,
            MatrixAction::Resize(100, 24, CursorReport::Position(18, 1)),
        ),
        (4, MatrixAction::Input(b"\x04")),
    ]);
    let base_live = last_live_frame(&baseline);
    let (_, base_top) = baseline
        .resize_marks
        .last()
        .copied()
        .expect("the lone resize measured once");
    assert_live_block(base_live, base_top, 24, 24);
    // Same live text at convergence; origins legitimately differ.
    let text = live_block(live)
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect::<Vec<_>>();
    let base_text = live_block(base_live)
        .into_iter()
        .map(|(_, bytes)| bytes)
        .collect::<Vec<_>>();
    assert_eq!(
        text, base_text,
        "resize storm and single resize converge to different live text"
    );
}
