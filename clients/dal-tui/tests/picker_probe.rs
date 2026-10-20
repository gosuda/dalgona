//! `PickerInteractionProbe`: model-picker navigation, filtering, commit, cancel.
//!
//! Each test drives the real terminal loop (`run_backend`) with a scripted
//! terminal whose reads are gated on previously captured frame bytes, so no
//! keystroke can race the picker opening. Every transition is asserted on the
//! captured frame writes (raw terminal bytes), never on picker internals.

#![cfg(test)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_core::{
    Answer, ApprovalMode, AutoCompaction, Chooser, ClientId, Command, ExtStatus, Gen, Mode,
    ModelRoute, Output, Page, PageReq, Reply, RequestId, Seq, SessionId, SessionInfo, SettingsView,
    Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Usage, UsageView, View, Workspace,
};
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_tui::picker::ModelOption;
use dal_tui::term::{CursorReport, TermIo};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

/// Probe replies from a plain terminal: no kitty keyboard, DA1 last.
const PROBE_REPLIES: &[&[u8]] = &[
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

/// Serializes loop runs: two probes never share the process clock.
static SERIAL: Mutex<()> = Mutex::new(());

/// What the fake host saw, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, event: impl Into<String>) {
        self.0.lock().expect("probe log").push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.0.lock().expect("probe log").clone()
    }

    fn has_prefix(&self, prefix: &str) -> bool {
        self.0
            .lock()
            .expect("probe log")
            .iter()
            .any(|event| event.starts_with(prefix))
    }
}

/// One captured frame write with the loop time it arrived at.
struct Frame {
    at: Duration,
    bytes: Vec<u8>,
}

/// When a scripted chunk becomes readable: a predicate over evidence so far.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
    Open,
    SawPicker,
    SawNav,
    SawFiltered,
    Committed,
    EscSettled,
}

/// One read's worth of input, held back until its gate opens.
struct Chunk {
    gate: Gate,
    bytes: Vec<u8>,
    esc: bool,
}

fn chunk(gate: Gate, bytes: &[u8]) -> Chunk {
    Chunk {
        gate,
        bytes: bytes.to_vec(),
        esc: false,
    }
}

struct State {
    chunks: VecDeque<Chunk>,
    frames: Vec<Frame>,
    esc_at: Option<Instant>,
    forced_quit: bool,
}

/// Scriptable terminal: gated reads, every frame byte captured.
struct ProbeIo {
    start: Instant,
    state: Mutex<State>,
    log: Log,
}

impl ProbeIo {
    fn new(script: Vec<Chunk>, log: Log) -> Self {
        Self {
            start: Instant::now(),
            state: Mutex::new(State {
                chunks: script.into(),
                frames: Vec::new(),
                esc_at: None,
                forced_quit: false,
            }),
            log,
        }
    }
}

struct Report {
    events: Vec<String>,
    frames: Vec<Frame>,
    esc_at: Option<Duration>,
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn gate_open(gate: Gate, frames: &[Frame], log: &Log, esc_at: Option<Instant>) -> bool {
    match gate {
        Gate::Open => true,
        Gate::SawPicker => frames
            .iter()
            .any(|frame| has(&frame.bytes, b"Pick a model")),
        Gate::SawNav => frames.iter().any(|frame| has(&frame.bytes, b"> beta")),
        Gate::SawFiltered => frames
            .iter()
            .any(|frame| has(&frame.bytes, b"beta") && !has(&frame.bytes, b"alpha")),
        Gate::Committed => log.has_prefix("set-model:"),
        Gate::EscSettled => esc_at.is_some_and(|at| at.elapsed() >= Duration::from_millis(600)),
    }
}

impl TermIo for ProbeIo {
    fn enable_raw(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.state
            .lock()
            .expect("probe io state")
            .frames
            .push(Frame {
                at: self.start.elapsed(),
                bytes: bytes.to_vec(),
            });
        Ok(())
    }

    fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        enum Decision {
            Emit(Vec<u8>, bool),
            ForceQuit,
            Wait,
        }
        let decision = {
            let mut state = self.state.lock().expect("probe io state");
            let elapsed = self.start.elapsed();
            assert!(
                elapsed <= Duration::from_secs(60),
                "picker probe watchdog: the terminal loop did not quit"
            );
            match state.chunks.front() {
                Some(front) if gate_open(front.gate, &state.frames, &self.log, state.esc_at) => {
                    let chunk = state.chunks.pop_front().expect("peeked probe chunk");
                    Decision::Emit(chunk.bytes, chunk.esc)
                }
                _ if elapsed > Duration::from_secs(25) && !state.forced_quit => {
                    state.forced_quit = true;
                    Decision::ForceQuit
                }
                _ => Decision::Wait,
            }
        };
        match decision {
            Decision::Emit(bytes, esc) => {
                if esc {
                    self.state.lock().expect("probe io state").esc_at = Some(Instant::now());
                }
                Ok(bytes)
            }
            Decision::ForceQuit => Ok(b"\x04".to_vec()),
            Decision::Wait => {
                std::thread::sleep(timeout.min(Duration::from_millis(30)));
                Ok(Vec::new())
            }
        }
    }

    fn raise_tstp(&self) {}

    fn cursor_position(&self, _timeout: Duration) -> std::io::Result<CursorReport> {
        Ok(CursorReport::Position(24, 1))
    }
}

struct Silent;

impl TuiSubscription for Silent {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        async { Ok(None) }.await
    }
}

/// A session whose `/model` run opens the model picker and which records
/// every submit, so assertions read typed host-visible outcomes.
#[derive(Clone)]
struct ProbeAgent {
    log: Log,
}

impl TuiAgent for ProbeAgent {
    type Subscription = Silent;

    fn session(&self) -> SessionId {
        SessionId::new_v7()
    }

    async fn view(&self, _page: PageReq) -> Result<View, TuiError> {
        async { Ok(idle_view()) }.await
    }

    async fn subscribe(&self, _after: Option<(Gen, Seq)>) -> Result<Silent, TuiError> {
        async { Ok(Silent) }.await
    }

    async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
        async {
            match command {
                Command::Run { name, .. } if name.as_ref() == "model" => {
                    self.log.push("run:model");
                    Ok(Reply::Choose {
                        chooser: Chooser::Model,
                        filter: "".into(),
                    })
                }
                Command::SetModel { model, .. } => {
                    self.log.push(format!("set-model:{}", model.id()));
                    Ok(Reply::Done(Output::Nothing))
                }
                _ => Ok(Reply::Done(Output::Nothing)),
            }
        }
        .await
    }

    async fn answer(&self, _id: RequestId, _answer: Answer) -> Result<(), TuiError> {
        async { Ok(()) }.await
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

#[derive(Clone)]
struct ProbeHost {
    agent: ProbeAgent,
}

impl TuiHost for ProbeHost {
    type Agent = ProbeAgent;

    async fn open(&self, _session: SessionRef, _client: ClientId) -> Result<ProbeAgent, TuiError> {
        async { Ok(self.agent.clone()) }.await
    }

    async fn commands(&self) -> Result<Arc<[dal_core::CommandSpec]>, TuiError> {
        async { Ok(Arc::from(Vec::new())) }.await
    }

    async fn close(&self, _id: SessionId) -> Result<(), TuiError> {
        async { Ok(()) }.await
    }

    async fn login(
        &self,
        _provider: &str,
        _method: Method,
        _io: LoginIo,
    ) -> Result<LoginOutcome, TuiError> {
        async { Err(TuiError::Terminal("the probe host has no providers".into())) }.await
    }

    async fn logout(&self, _provider: Option<&str>) -> Result<Vec<Box<str>>, TuiError> {
        async { Ok(Vec::new()) }.await
    }

    async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, TuiError> {
        async { Ok(Vec::new()) }.await
    }
}

fn idle_view() -> View {
    View {
        r#gen: Gen::new(std::num::NonZeroU64::MIN),
        seq: Seq::new(std::num::NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
                .expect("fixture session id is valid"),
            name: None,
            preview: "".into(),
            workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
            updated_at: Timestamp::UNIX_EPOCH,
            created_at: Some(Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(std::num::NonZeroU64::MIN)),
            message_count: None,
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
            scoped_models: None,
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
    }
}

fn probe_options(rt: &tokio::runtime::Handle) -> TuiOptions {
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

fn probe_models() -> Vec<ModelOption> {
    [
        ("alpha", "t-alpha"),
        ("beta", "t-beta"),
        ("gamma", "t-gamma"),
    ]
    .into_iter()
    .map(|(name, id)| ModelOption {
        label: format!("{name} · {id}"),
        command: Command::SetModel {
            model: ModelRoute::Synthetic { id: id.into() },
            save: dal_core::command::Save::SessionAndDefault,
        },
        scoped_id: None,
    })
    .collect()
}

fn scripted(script: Vec<Chunk>) -> Vec<Chunk> {
    PROBE_REPLIES
        .iter()
        .map(|reply| chunk(Gate::Open, reply))
        .chain(script)
        .collect()
}

/// Open, arrow-navigate, type-to-filter, then Enter to commit.
fn commit_script() -> Vec<Chunk> {
    scripted(vec![
        chunk(Gate::Open, b"/model\r"),
        chunk(Gate::SawPicker, b"\x1b[B"),
        chunk(Gate::SawNav, b"b"),
        chunk(Gate::SawFiltered, b"\r"),
        chunk(Gate::Committed, b"\x04"),
    ])
}

/// Open, then Esc to cancel without committing.
fn cancel_script() -> Vec<Chunk> {
    let mut script = scripted(vec![
        chunk(Gate::Open, b"/model\r"),
        chunk(Gate::SawPicker, b"\x1b"),
        chunk(Gate::EscSettled, b"\x04"),
    ]);
    if let Some(esc) = script
        .iter_mut()
        .find(|chunk| chunk.gate == Gate::SawPicker)
    {
        esc.esc = true;
    }
    script
}

/// Runs the terminal loop against the probe host and returns host-visible
/// submits plus every captured frame write.
fn drive(script: Vec<Chunk>) -> Report {
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
    let io = ProbeIo::new(script, log);
    dal_tui::run_backend(&host, &opts, &io, || Ok(probe_models()))
        .expect("the picker probe loop exits cleanly");
    let mut state = io.state.lock().expect("probe io state");
    Report {
        events: io.log.events(),
        frames: std::mem::take(&mut state.frames),
        esc_at: state
            .esc_at
            .map(|at| at.saturating_duration_since(io.start)),
    }
}

/// Index of the first frame satisfying `wanted`, if any.
fn first(frames: &[Frame], wanted: impl Fn(&[u8]) -> bool) -> Option<usize> {
    frames.iter().position(|frame| wanted(&frame.bytes))
}

#[test]
fn picker_navigate_filter_commit() {
    let report = drive(commit_script());
    let frames = &report.frames;
    assert!(
        !frames.is_empty(),
        "the loop wrote no frame bytes at all ({} events)",
        report.events.len()
    );
    let open = first(frames, |bytes| has(bytes, b"Pick a model"))
        .expect("no frame ever rendered the picker title");
    let initial = first(frames, |bytes| has(bytes, b"> alpha"))
        .expect("no frame ever rendered the initial selection");
    let nav = first(frames, |bytes| has(bytes, b"> beta"))
        .expect("no frame ever rendered the arrow-moved selection");
    let filtered = first(frames, |bytes| has(bytes, b"beta") && !has(bytes, b"alpha"))
        .expect("no frame ever rendered the filtered candidate list");
    assert!(
        open < nav,
        "navigation bytes must follow the picker open (open {open}, nav {nav})"
    );
    assert!(
        initial < nav,
        "the moved selection must follow the initial one (initial {initial}, nav {nav})"
    );
    assert!(
        nav < filtered,
        "the filter must narrow the list after navigation (nav {nav}, filtered {filtered})"
    );
    assert!(
        report.events.iter().any(|event| event == "run:model"),
        "the picker never opened via /model: {:?}",
        report.events
    );
    assert!(
        report
            .events
            .iter()
            .any(|event| event == "set-model:t-beta"),
        "Enter did not commit the filtered choice: {:?}",
        report.events
    );
    let closed = frames
        .iter()
        .enumerate()
        .skip(filtered + 1)
        .find(|(_, frame)| has(&frame.bytes, b"enter send") && !has(&frame.bytes, b"Pick a model"))
        .map(|(index, _)| index)
        .expect("no post-commit frame restored the composer");
    let last_hint = frames
        .iter()
        .rposition(|frame| has(&frame.bytes, b"enter confirm"))
        .expect("no frame ever rendered the picker hint");
    assert!(
        last_hint < closed,
        "the picker repainted after commit (last hint {last_hint}, closed {closed})"
    );
}

#[test]
fn picker_escape_cancels_without_commit() {
    let report = drive(cancel_script());
    let frames = &report.frames;
    assert!(
        !frames.is_empty(),
        "the loop wrote no frame bytes at all ({} events)",
        report.events.len()
    );
    assert!(
        first(frames, |bytes| has(bytes, b"Pick a model")).is_some(),
        "no frame ever rendered the picker title"
    );
    assert!(
        first(frames, |bytes| has(bytes, b"> alpha")).is_some(),
        "no frame ever rendered the initial selection"
    );
    let esc_at = report.esc_at.expect("the escape chunk was never consumed");
    assert_eq!(
        report.events,
        vec!["run:model".to_owned()],
        "cancel committed or dropped input: {:?}",
        report.events
    );
    let closed = frames
        .iter()
        .position(|frame| {
            frame.at > esc_at
                && has(&frame.bytes, b"enter send")
                && !has(&frame.bytes, b"Pick a model")
        })
        .expect("no post-escape frame restored the composer");
    let last_hint = frames
        .iter()
        .rposition(|frame| has(&frame.bytes, b"enter confirm"))
        .expect("no frame ever rendered the picker hint");
    assert!(
        last_hint < closed,
        "the picker repainted after cancel (last hint {last_hint}, closed {closed})"
    );
}
