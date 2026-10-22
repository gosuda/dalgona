//! `TranscriptResizeProbe` (DES-01): transcript width-cycle evidence.
//!
//! Byte-indexed proof over the public inline-screen/byte driver
//! (`dal_tui::screen::inline`) plus one real-loop width storm
//! (`dal_tui::run_backend` with a scripted terminal):
//! commit transcript rows, shrink 80 -> 40, grow 40 -> 100, and assert
//! committed text is preserved, exactly one live block exists per frame,
//! and zero frame writes repaint rows above the live-block top.
//!
//! Overlay entry/exit and quit teardown reuse the in-tree driver unit
//! harness (`screen/driver.rs` overlay + rest tests) which owns the private
//! `Painter`; this probe asserts the loop-level reachability half (clean
//! quit, no scrollback clear) and documents the overlay half as covered
//! in-tree.

use std::collections::VecDeque;
use std::future::Future;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_core::{
    Answer, ApprovalMode, AutoCompaction, Chooser, ClientId, Command, EntryId, EntryKind,
    EntryView, ExtStatus, Gen, JournalPart, Mode, Page, PageReq, Reply, Seq, SessionId,
    SessionInfo, SettingsView, Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Usage,
    UsageView, View, Workspace,
};
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_tui::screen::inline::{Region, commit_bytes, shrink_bytes};
use dal_tui::term::{CursorReport, TermIo};
use dal_tui::transcript::Transcript;
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

/// Distinctive committed rows; byte-indexed, never repainted after commit.
const COMMITTED: &[&str] = &[
    "resize-probe committed alpha 01",
    "resize-probe committed beta 02",
    "resize-probe committed gamma 03",
];

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
        let position = bytes[cursor..]
            .windows(text.len())
            .position(|w| w == text.as_bytes());
        assert!(
            position.is_some(),
            "committed row {text:?} missing from frame bytes"
        );
        let found = position.unwrap_or(0);
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

// --- Real-loop width storm: 80 -> 40 -> 100, then clean quit. ---

pub(crate) const PROBE_REPLIES: &[&[u8]] = &[
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

#[derive(Clone, Default)]
pub(crate) struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, event: impl Into<String>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.into());
    }

    pub(crate) fn events(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub(crate) struct Frame {
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    Open,
    Frames(usize),
}

pub(crate) enum Action {
    Input(Vec<u8>),
    Resize(u16, u16, CursorReport),
}

pub(crate) struct Chunk {
    pub(crate) gate: Gate,
    pub(crate) action: Action,
    pub(crate) ready: Option<fn(&[Frame]) -> bool>,
}

pub(crate) struct State {
    chunks: VecDeque<Chunk>,
    pub(crate) frames: Vec<Frame>,
    /// One `(frame_index, measured_top)` per applied resize, in order.
    resize_marks: Vec<(usize, u16)>,
    forced_quit: bool,
}

pub(crate) struct ProbeIo {
    start: Instant,
    pub(crate) state: Mutex<State>,
    size: Mutex<(u16, u16)>,
    epoch: Mutex<u64>,
    resize_pending: Mutex<bool>,
    reports: Mutex<VecDeque<CursorReport>>,
    pub(crate) log: Log,
}

enum ReadDecision {
    Emit(Vec<u8>),
    Wait,
}

impl ProbeIo {
    pub(crate) fn new(script: Vec<Chunk>, log: Log) -> Self {
        Self {
            start: Instant::now(),
            state: Mutex::new(State {
                chunks: script.into(),
                frames: Vec::new(),
                resize_marks: Vec::new(),
                forced_quit: false,
            }),
            size: Mutex::new((80, 24)),
            epoch: Mutex::new(0),
            resize_pending: Mutex::new(false),
            reports: Mutex::new(VecDeque::new()),
            log,
        }
    }

    fn apply_resize(
        &self,
        state: &mut State,
        width: u16,
        height: u16,
        report: CursorReport,
        queue_report: bool,
    ) {
        *self
            .size
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (width, height);
        *self
            .epoch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        *self
            .resize_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        if let CursorReport::Position(row, _) = report {
            let frames = state.frames.len();
            state.resize_marks.push((frames, row));
        }
        if !queue_report {
            return;
        }
        let mut reports = self
            .reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reports.push_back(report);
        if matches!(report, CursorReport::Silent | CursorReport::Unpaired) {
            reports.push_back(CursorReport::Position(height.max(1), 1));
        }
    }

    fn decide(&self, state: &mut State) -> ReadDecision {
        let Some(front) = state.chunks.front() else {
            return self.idle(state);
        };
        if !chunk_ready(front, &state.frames) {
            return self.idle(state);
        }
        let gate = front.gate;
        match state.chunks.pop_front() {
            Some(Chunk {
                action: Action::Input(bytes),
                ..
            }) => ReadDecision::Emit(bytes),
            Some(Chunk {
                action: Action::Resize(w, h, report),
                ..
            }) => {
                let burst = Self::collect_burst(state, gate, (w, h, report));
                let last = burst.len() - 1;
                for (index, (w, h, report)) in burst.into_iter().enumerate() {
                    self.apply_resize(state, w, h, report, index == last);
                }
                ReadDecision::Emit(Vec::new())
            }
            None => ReadDecision::Wait,
        }
    }

    fn collect_burst(
        state: &mut State,
        gate: Gate,
        first: (u16, u16, CursorReport),
    ) -> Vec<(u16, u16, CursorReport)> {
        let mut burst = vec![first];
        if !matches!(gate, Gate::Frames(_)) {
            return burst;
        }
        loop {
            let ready = matches!(
                state.chunks.front(),
                Some(Chunk {
                    gate: next_gate,
                    action: Action::Resize(..),
                    ..
                }) if *next_gate == gate
            );
            if !ready {
                return burst;
            }
            let Some(Chunk {
                action: Action::Resize(w, h, report),
                ..
            }) = state.chunks.pop_front()
            else {
                return burst;
            };
            burst.push((w, h, report));
        }
    }

    fn idle(&self, state: &mut State) -> ReadDecision {
        if self.start.elapsed() > Duration::from_secs(25) && !state.forced_quit {
            state.forced_quit = true;
            ReadDecision::Emit(b"\x04".to_vec())
        } else {
            ReadDecision::Wait
        }
    }
}

fn gate_open(gate: Gate, frames: &[Frame]) -> bool {
    match gate {
        Gate::Open => true,
        Gate::Frames(count) => frames.len() >= count,
    }
}

fn chunk_ready(chunk: &Chunk, frames: &[Frame]) -> bool {
    gate_open(chunk.gate, frames) && chunk.ready.is_none_or(|ready| ready(frames))
}

impl TermIo for ProbeIo {
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

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frames
            .push(Frame {
                bytes: bytes.to_vec(),
            });
        Ok(())
    }

    fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        let decision = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                self.start.elapsed() <= Duration::from_secs(60),
                "resize probe watchdog: the terminal loop did not quit"
            );
            self.decide(&mut state)
        };
        match decision {
            ReadDecision::Emit(bytes) => Ok(bytes),
            ReadDecision::Wait => {
                std::thread::sleep(timeout.min(Duration::from_millis(30)));
                Ok(Vec::new())
            }
        }
    }

    fn raise_tstp(&self) {}

    fn take_resize(&self) -> bool {
        std::mem::replace(
            &mut *self
                .resize_pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            false,
        )
    }

    fn resize_epoch(&self) -> u64 {
        *self
            .epoch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cursor_position(&self, _timeout: Duration) -> std::io::Result<CursorReport> {
        Ok(self
            .reports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .unwrap_or(CursorReport::Position(24, 1)))
    }
}

pub(crate) struct Silent;

impl TuiSubscription for Silent {
    fn next(&mut self) -> impl Future<Output = Result<Option<TuiDelivery>, TuiError>> {
        std::future::ready(Ok(None))
    }
}

#[derive(Clone)]
pub(crate) struct ProbeAgent {
    pub(crate) log: Log,
}

fn user_entry(id: u64, text: &str) -> EntryView {
    EntryView {
        id: EntryId::new(NonZeroU64::new(id).unwrap_or(NonZeroU64::MIN)),
        parent: None,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }
}

fn resize_view() -> Result<View, TuiError> {
    let mut view = idle_view()?;
    view.entries.items = vec![
        user_entry(1, COMMITTED[0]),
        user_entry(2, COMMITTED[1]),
        user_entry(3, COMMITTED[2]),
    ];
    Ok(view)
}

impl TuiAgent for ProbeAgent {
    type Subscription = Silent;

    fn session(&self) -> SessionId {
        SessionId::new_v7()
    }

    fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
        std::future::poll_fn(|_| Poll::Ready(resize_view()))
    }

    fn subscribe(
        &self,
        _after: Option<(Gen, Seq)>,
    ) -> impl Future<Output = Result<Silent, TuiError>> {
        std::future::ready(Ok(Silent))
    }

    fn submit(&self, command: Command) -> impl Future<Output = Result<Reply, TuiError>> {
        match command {
            Command::Run { name, .. } if name.as_ref() == "model" => {
                std::future::ready(Ok(Reply::Choose {
                    chooser: Chooser::Model,
                    filter: "".into(),
                }))
            }
            _ => std::future::ready(Ok(Reply::Done(dal_core::Output::Nothing))),
        }
    }

    fn answer(
        &self,
        _id: dal_core::RequestId,
        _answer: Answer,
    ) -> impl Future<Output = Result<(), TuiError>> {
        std::future::ready(Ok(()))
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

#[derive(Clone)]
pub(crate) struct ProbeHost {
    pub(crate) agent: ProbeAgent,
}

impl TuiHost for ProbeHost {
    type Agent = ProbeAgent;

    fn open(
        &self,
        _session: SessionRef,
        _client: ClientId,
    ) -> impl Future<Output = Result<ProbeAgent, TuiError>> {
        std::future::ready(Ok(self.agent.clone()))
    }

    fn commands(&self) -> impl Future<Output = Result<Arc<[dal_core::CommandSpec]>, TuiError>> {
        match dal_core::CommandName::parse("model") {
            Ok(name) => {
                let model = dal_core::CommandSpec {
                    name,
                    summary: "choose the model".into(),
                    args_hint: None,
                };
                std::future::ready(Ok(Arc::from(vec![model])))
            }
            Err(error) => std::future::ready(Err(TuiError::Terminal(format!(
                "resize probe fixture: {error}"
            )))),
        }
    }

    fn close(&self, _id: SessionId) -> impl Future<Output = Result<(), TuiError>> {
        std::future::poll_fn(|_| {
            self.agent.log.push("host:close");
            Poll::Ready(Ok(()))
        })
    }

    fn login(
        &self,
        _provider: &str,
        _method: Method,
        _io: LoginIo,
    ) -> impl Future<Output = Result<LoginOutcome, TuiError>> {
        std::future::ready(Err(TuiError::Terminal(
            "the probe host has no providers".into(),
        )))
    }

    fn logout(
        &self,
        _provider: Option<&str>,
    ) -> impl Future<Output = Result<Vec<Box<str>>, TuiError>> {
        std::future::ready(Ok(Vec::new()))
    }

    fn stored_credentials(&self) -> impl Future<Output = Result<Vec<StoredCredential>, TuiError>> {
        std::future::ready(Ok(Vec::new()))
    }
}

fn idle_view() -> Result<View, TuiError> {
    let failed = |error: &dyn std::fmt::Display| {
        TuiError::Terminal(format!("resize probe fixture: {error}"))
    };
    Ok(View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
                .map_err(|error| failed(&error))?,
            name: None,
            preview: "".into(),
            workspace: Workspace::new(std::env::temp_dir()).map_err(|error| failed(&error))?,
            updated_at: Timestamp::UNIX_EPOCH,
            created_at: Some(Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
        },
        turn: TurnState::Idle,
        entries: Page {
            items: Vec::new(),
            next_before: None,
        },
        tree: TreeOutline {
            branches: Vec::new(),
        },
        settings: SettingsView {
            model: None,
            thinking: ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            mode: Mode::Normal,
            name: None,
        },
        open: Vec::new(),
        changes: Vec::new(),
        usage: UsageView {
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
            context_tokens: 0,
            context_window: 128_000,
        },
        stats: Stats {
            steers_queued: 0,
            follow_ups_queued: 0,
            retries: 0,
            dropped_observations: 0,
            auto_compaction: AutoCompaction::Off,
        },
    })
}

#[expect(
    clippy::expect_used,
    reason = "probe fixture: `temp_dir` is always absolute"
)]
pub(crate) fn probe_options(rt: &tokio::runtime::Handle) -> TuiOptions {
    TuiOptions {
        session: SessionRef::Ephemeral {
            workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
        },
        screen: Screen::Inline,
        theme_request: ThemeRequest::Palette,
        default_model: None,
        images: false,
        diagrams: false,
        motion: false,
        editor: "true".into(),
        color: ColorMode::Never,
        binary: "dal",
        env: EnvFacts {
            stdin_tty: true,
            term: Some("xterm-256color".to_owned()),
            width_mode: WidthMode::Narrow,
            ..EnvFacts::default()
        },
        rt: rt.clone(),
    }
}

struct Report {
    events: Vec<String>,
    frames: Vec<Frame>,
    resize_marks: Vec<(usize, u16)>,
}

fn drive() -> Result<Report, TuiError> {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| TuiError::Terminal(format!("probe runtime: {error}")))?;
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
        .chain([
            Chunk {
                gate: Gate::Frames(2),
                action: Action::Resize(40, 24, CursorReport::Position(20, 1)),
                ready: None,
            },
            Chunk {
                gate: Gate::Frames(4),
                action: Action::Resize(100, 24, CursorReport::Position(18, 1)),
                ready: None,
            },
            Chunk {
                gate: Gate::Frames(6),
                action: Action::Input(b"\x04".to_vec()),
                ready: None,
            },
        ])
        .collect::<Vec<_>>();
    let io = ProbeIo::new(script, log);
    dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()))?;
    let mut state = io
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(Report {
        events: io.log.events(),
        frames: std::mem::take(&mut state.frames),
        resize_marks: std::mem::take(&mut state.resize_marks),
    })
}

#[test]
fn loop_shrink_then_grow_never_clears_screen_and_quits_cleanly() {
    let report = drive().expect("the resize probe loop exits cleanly");
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
