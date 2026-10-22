//! Resize-review R5 behavioral matrix.
//!
//! This file deliberately reuses the real scripted terminal harness from
//! `resize_probe.rs`; unlike unit tests, every assertion below observes the
//! bytes emitted by `run_backend`.

#[path = "resize_probe.rs"]
mod resize_probe;

use dal_core::{Command, ModelRoute};
use dal_tui::picker::ModelOption;
use resize_probe::{
    Action, Chunk, Frame, Gate, Log, PROBE_REPLIES, ProbeAgent, ProbeHost, ProbeIo, SERIAL,
    probe_options,
};

pub(crate) enum MatrixAction {
    Input(&'static [u8]),
    Resize(u16, u16, dal_tui::term::CursorReport),
    ResizeWhenPicker(u16, u16, dal_tui::term::CursorReport),
}

fn picker_painted(frames: &[Frame]) -> bool {
    frames
        .iter()
        .any(|frame| has(&frame.bytes, b"Pick a model"))
}

pub(crate) struct MatrixReport {
    pub frames: Vec<Vec<u8>>,
    pub events: Vec<String>,
    pub size: (u16, u16),
}

pub(crate) fn matrix_drive(
    actions: Vec<(Gate, MatrixAction)>,
) -> Result<MatrixReport, dal_tui::TuiError> {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| dal_tui::TuiError::Terminal(format!("resize matrix runtime: {error}")))?;
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
            ready: None,
        })
        .chain(actions.into_iter().map(|(gate, action)| match action {
            MatrixAction::Input(bytes) => Chunk {
                gate,
                action: Action::Input(bytes.to_vec()),
                ready: None,
            },
            MatrixAction::Resize(width, height, report) => Chunk {
                gate,
                action: Action::Resize(width, height, report),
                ready: None,
            },
            MatrixAction::ResizeWhenPicker(width, height, report) => Chunk {
                gate,
                action: Action::Resize(width, height, report),
                ready: Some(picker_painted),
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
            },
            ModelOption {
                label: "beta · t-beta".into(),
                command: Command::SetModel {
                    model: ModelRoute::Synthetic {
                        id: "t-beta".into(),
                    },
                    save: dal_core::command::Save::SessionAndDefault,
                },
            },
        ])
    })?;
    let mut state = io
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(MatrixReport {
        frames: state.frames.drain(..).map(|frame| frame.bytes).collect(),
        events: io.log.events(),
        size: io.size().unwrap_or((0, 0)),
    })
}

use dal_tui::term::{CursorReport, TermIo};

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
        (Gate::Frames(2), MatrixAction::Input("日本語🙂".as_bytes())),
        (
            Gate::Frames(3),
            MatrixAction::Resize(10, 24, CursorReport::Position(20, 1)),
        ),
        (
            Gate::Frames(4),
            MatrixAction::Resize(80, 24, CursorReport::Position(18, 1)),
        ),
        (Gate::Frames(6), MatrixAction::Input(b"\x7f\x7f\x7f\x7f")),
        (Gate::Frames(6), MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
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
        (Gate::Frames(2), MatrixAction::Input(b"/model\r")),
        (
            Gate::Open,
            MatrixAction::ResizeWhenPicker(11, 24, CursorReport::Position(20, 1)),
        ),
        (Gate::Open, MatrixAction::Input(b"\x1b[B")),
        (
            Gate::Open,
            MatrixAction::Resize(80, 7, CursorReport::Position(6, 1)),
        ),
        (
            Gate::Open,
            MatrixAction::Resize(80, 24, CursorReport::Position(18, 1)),
        ),
        (Gate::Open, MatrixAction::Input(b"\x1b[27;1u")),
        (Gate::Open, MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
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
        (Gate::Frames(2), MatrixAction::Input(b"/model\r")),
        (
            Gate::Open,
            MatrixAction::ResizeWhenPicker(40, 12, CursorReport::Position(10, 1)),
        ),
        (Gate::Open, MatrixAction::Input(b"\x1b[27;1u")),
        (Gate::Open, MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
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
        (Gate::Frames(2), MatrixAction::Input(b"/model\r")),
        (
            Gate::Open,
            MatrixAction::ResizeWhenPicker(40, 12, CursorReport::Silent),
        ),
        (Gate::Open, MatrixAction::Input(b"\x1b[27;1u")),
        (Gate::Open, MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
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
        (
            Gate::Frames(2),
            MatrixAction::Resize(60, 24, CursorReport::Position(20, 1)),
        ),
        (
            Gate::Frames(2),
            MatrixAction::Resize(40, 20, CursorReport::Position(16, 1)),
        ),
        (
            Gate::Frames(2),
            MatrixAction::Resize(100, 24, CursorReport::Position(18, 1)),
        ),
        (Gate::Frames(6), MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
    assert_safe_frames(&report);
    assert_committed_once(&report);
    assert_eq!(
        report.size,
        (100, 24),
        "storm did not settle on its final size"
    );
    let final_paint = last_paint(&report).expect("storm captured no paint frame");
    let baseline = matrix_drive(vec![
        (
            Gate::Frames(2),
            MatrixAction::Resize(100, 24, CursorReport::Position(18, 1)),
        ),
        (Gate::Frames(4), MatrixAction::Input(b"\x04")),
    ])
    .expect("resize matrix loop exits cleanly");
    assert_eq!(baseline.size, (100, 24));
    assert_eq!(
        final_paint,
        last_paint(&baseline).expect("baseline captured no paint frame"),
        "storm final paint differs from a single resize to its last size"
    );
}

fn last_paint(report: &MatrixReport) -> Option<&[u8]> {
    report
        .frames
        .iter()
        .rev()
        .find(|frame| has(frame, b"\x1b[?2026h"))
        .map(Vec::as_slice)
}

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

struct PanicIo {
    size: Mutex<(u16, u16)>,
    replies: Mutex<VecDeque<Vec<u8>>>,
    idle_reads: AtomicUsize,
    panicked: AtomicBool,
}

impl PanicIo {
    fn new() -> Self {
        Self {
            size: Mutex::new((80, 24)),
            replies: Mutex::new(PROBE_REPLIES.iter().map(|reply| reply.to_vec()).collect()),
            idle_reads: AtomicUsize::new(0),
            panicked: AtomicBool::new(false),
        }
    }
}

impl TermIo for PanicIo {
    fn enable_raw(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok(*self
            .size
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn write(&self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    #[expect(
        clippy::panic,
        reason = "panic-hook proof: the scripted terminal panic must unwind through run_backend to prove the hook runs"
    )]
    fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        if let Some(reply) = self
            .replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            return Ok(reply);
        }
        if self.idle_reads.fetch_add(1, Ordering::SeqCst) >= 4 {
            self.panicked.store(true, Ordering::SeqCst);
            panic!("panic_hook_unwind: scripted terminal panic inside the run loop");
        }
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {}

    fn cursor_position(&self, _timeout: Duration) -> std::io::Result<CursorReport> {
        Ok(CursorReport::Position(24, 1))
    }
}

#[expect(
    clippy::panic,
    reason = "panic-hook proof: the second panic must be a real panic to prove the restored hook runs"
)]
fn panic_hook_child() -> Result<(), dal_tui::TuiError> {
    static HOOK_HITS: AtomicUsize = AtomicUsize::new(0);
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_info| {
        HOOK_HITS.fetch_add(1, Ordering::SeqCst);
    }));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            dal_tui::TuiError::Terminal(format!("panic-hook child runtime: {error}"))
        })?;
    let log = Log::default();
    let host = ProbeHost {
        agent: ProbeAgent { log: log.clone() },
    };
    let opts = probe_options(runtime.handle());
    let io = PanicIo::new();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    }));
    assert!(
        caught.is_err(),
        "the scripted loop panic did not propagate out of run_backend"
    );
    assert!(
        io.panicked.load(Ordering::SeqCst),
        "the scripted terminal panic never fired inside the loop"
    );
    let before = HOOK_HITS.load(Ordering::SeqCst);
    let _ = std::panic::catch_unwind(|| panic!("second panic probes the restored hook"));
    let after = HOOK_HITS.load(Ordering::SeqCst);
    assert!(
        after > before,
        "the previous hook was not restored after the caught loop panic"
    );
    std::panic::set_hook(original);
    println!("panic-hook-child-ok");
    Ok(())
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "panic-hook proof: the test isolates the hook-restore probe in a child test executable via its own env flag"
)]
fn panic_in_loop_restores_hook_without_aborting() {
    if std::env::var_os("DAL_TUI_PANIC_HOOK_CHILD").is_some() {
        panic_hook_child().expect("panic-hook child setup");
        return;
    }
    let exe = std::env::current_exe().expect("current test executable");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "panic_in_loop_restores_hook_without_aborting",
            "--nocapture",
        ])
        .env("DAL_TUI_PANIC_HOOK_CHILD", "1")
        .output()
        .expect("spawn panic-hook child");
    assert!(
        output.status.success(),
        "child aborted or failed (SIGABRT regression): status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("panic-hook-child-ok"),
        "child never reached the restored-hook assertion"
    );
}
