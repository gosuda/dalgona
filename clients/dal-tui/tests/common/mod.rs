//! Shared resize harness for scripted width tests.

use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
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
use dal_tui::term::{CursorReport, TermIo};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

/// Distinctive committed rows; byte-indexed, never repainted after commit.
pub(crate) const COMMITTED: &[&str] = &[
    "resize-probe committed alpha 01",
    "resize-probe committed beta 02",
    "resize-probe committed gamma 03",
];

pub(crate) const PROBE_REPLIES: &[&[u8]] = &[
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

/// How long the key decoder holds a lone `ESC` byte (`keys/decoder.rs`).
const ESC_WINDOW: Duration = Duration::from_millis(50);

/// How long a `/model` submit may take to reach a frame as the picker.
const REPLY_LIMIT: Duration = Duration::from_secs(2);

/// The bytes a frame carries once the model picker is on screen; the same
/// bytes the scenario assertions look for.
const MODEL_PICKER_READY: &[u8] = b"Pick a model";

#[derive(Clone, Default)]
pub(crate) struct Log(pub(crate) Arc<Mutex<Vec<String>>>);

impl Log {
    pub(crate) fn push(&self, event: impl Into<String>) {
        self.0.lock().expect("probe log").push(event.into());
    }

    pub(crate) fn events(&self) -> Vec<String> {
        self.0.lock().expect("probe log").clone()
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
}

/// Input the loop has not finished digesting; later chunks wait.
#[derive(Clone, Copy)]
enum Hold {
    /// A chunk ended in a lone `ESC`, which the decoder reports only after
    /// its window closes.
    Esc { until: Instant },
    /// A chunk submitted a line; the command answers on its own thread, so
    /// the next chunk waits for a frame showing the model picker.
    Reply { until: Instant },
}

/// True once some recorded frame shows the model picker.
fn picker_shown(frames: &[Frame]) -> bool {
    frames.iter().any(|frame| {
        frame
            .bytes
            .windows(MODEL_PICKER_READY.len())
            .any(|window| window == MODEL_PICKER_READY)
    })
}

pub(crate) struct State {
    chunks: VecDeque<Chunk>,
    pub(crate) frames: Vec<Frame>,
    /// One `(frame_index, measured_top)` per applied resize, in order.
    pub(crate) resize_marks: Vec<(usize, u16)>,
    forced_quit: bool,
    hold: Option<Hold>,
}

impl State {
    /// False once nothing is being digested. An expired `Esc` hold clears on
    /// the read that observes it, so the decoder tick runs after the close.
    fn holding(&mut self) -> bool {
        let now = Instant::now();
        match self.hold {
            None => false,
            Some(Hold::Esc { until }) => {
                if now >= until {
                    self.hold = None;
                }
                true
            }
            Some(Hold::Reply { .. }) => {
                if picker_shown(&self.frames) {
                    self.hold = None;
                    false
                } else {
                    true
                }
            }
        }
    }

    /// True when a submit outlasted its deadline with no picker frame: the
    /// caller fails the run instead of scripting into an unknown state.
    fn reply_expired(&self) -> bool {
        matches!(self.hold, Some(Hold::Reply { until }) if Instant::now() >= until)
            && !picker_shown(&self.frames)
    }
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

impl ProbeIo {
    pub(crate) fn new(script: Vec<Chunk>, log: Log) -> Self {
        Self {
            start: Instant::now(),
            state: Mutex::new(State {
                chunks: script.into(),
                frames: Vec::new(),
                resize_marks: Vec::new(),
                forced_quit: false,
                hold: None,
            }),
            size: Mutex::new((80, 24)),
            epoch: Mutex::new(0),
            resize_pending: Mutex::new(false),
            reports: Mutex::new(VecDeque::new()),
            log,
        }
    }
}

fn gate_open(gate: Gate, frames: &[Frame]) -> bool {
    match gate {
        Gate::Open => true,
        Gate::Frames(count) => frames.len() >= count,
    }
}

impl TermIo for ProbeIo {
    fn enable_raw(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok(*self.size.lock().expect("probe size"))
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.state.lock().expect("probe state").frames.push(Frame {
            bytes: bytes.to_vec(),
        });
        Ok(())
    }

    fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        enum Decision {
            Emit(Vec<u8>),
            Wait,
        }
        let decision = {
            let mut state = self.state.lock().expect("probe state");
            assert!(
                self.start.elapsed() <= Duration::from_secs(60),
                "resize probe watchdog: the terminal loop did not quit"
            );
            let holding = state.holding();
            if state.reply_expired() {
                let observed = state.frames.len();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "probe input hold expired: no frame showed the model picker after the /model submit ({observed} frames observed)"
                    ),
                ));
            }
            match state.chunks.front() {
                Some(front) if !holding && gate_open(front.gate, &state.frames) => {
                    let chunk = state.chunks.pop_front().expect("peeked probe chunk");
                    match chunk.action {
                        Action::Input(bytes) => {
                            state.hold = match bytes.last() {
                                Some(&0x1b) => Some(Hold::Esc {
                                    until: Instant::now() + ESC_WINDOW,
                                }),
                                Some(&b'\r') => Some(Hold::Reply {
                                    until: Instant::now() + REPLY_LIMIT,
                                }),
                                _ => None,
                            };
                            Decision::Emit(bytes)
                        }
                        Action::Resize(w, h, report) => {
                            *self.size.lock().expect("probe size") = (w, h);
                            *self.epoch.lock().expect("probe epoch") += 1;
                            *self.resize_pending.lock().expect("pending") = true;
                            if let CursorReport::Position(row, _) = report {
                                let frames = state.frames.len();
                                state.resize_marks.push((frames, row));
                            }
                            self.reports.lock().expect("reports").push_back(report);
                            Decision::Emit(Vec::new())
                        }
                    }
                }
                _ => {
                    if !holding
                        && self.start.elapsed() > Duration::from_secs(25)
                        && !state.forced_quit
                    {
                        state.forced_quit = true;
                        Decision::Emit(b"\x04".to_vec())
                    } else {
                        Decision::Wait
                    }
                }
            }
        };
        match decision {
            Decision::Emit(bytes) => Ok(bytes),
            Decision::Wait => {
                std::thread::sleep(timeout.min(Duration::from_millis(30)));
                Ok(Vec::new())
            }
        }
    }

    fn raise_tstp(&self) {}

    fn take_resize(&self) -> bool {
        std::mem::replace(&mut *self.resize_pending.lock().expect("pending"), false)
    }

    fn resize_epoch(&self) -> u64 {
        *self.epoch.lock().expect("probe epoch")
    }

    fn cursor_position(&self, _timeout: Duration) -> std::io::Result<CursorReport> {
        Ok(self
            .reports
            .lock()
            .expect("reports")
            .pop_front()
            .unwrap_or(CursorReport::Position(24, 1)))
    }
}

pub(crate) struct Silent;

impl TuiSubscription for Silent {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        async { Ok(None) }.await
    }
}

#[derive(Clone)]
pub(crate) struct ProbeAgent {
    pub(crate) log: Log,
}

fn user_entry(id: u64, text: &str) -> EntryView {
    EntryView {
        id: EntryId::new(NonZeroU64::new(id).expect("entry id")),
        parent: None,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }
}

fn resize_view() -> View {
    let mut view = idle_view();
    view.entries.items = vec![
        user_entry(1, COMMITTED[0]),
        user_entry(2, COMMITTED[1]),
        user_entry(3, COMMITTED[2]),
    ];
    view
}

impl TuiAgent for ProbeAgent {
    type Subscription = Silent;

    fn session(&self) -> SessionId {
        SessionId::new_v7()
    }

    async fn view(&self, _page: PageReq) -> Result<View, TuiError> {
        async { Ok(resize_view()) }.await
    }

    async fn subscribe(&self, _after: Option<(Gen, Seq)>) -> Result<Silent, TuiError> {
        async { Ok(Silent) }.await
    }

    /// `/model` is answered with the model chooser, as a real session does;
    /// the loop then opens the picker from its own model source. Every other
    /// command completes with nothing.
    async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
        async {
            match command {
                Command::Run { name, .. } if name.as_ref() == "model" => Ok(Reply::Choose {
                    chooser: Chooser::Model,
                    filter: "".into(),
                }),
                _ => Ok(Reply::Done(dal_core::Output::Nothing)),
            }
        }
        .await
    }

    async fn answer(&self, _id: dal_core::RequestId, _answer: Answer) -> Result<(), TuiError> {
        async { Ok(()) }.await
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

    async fn open(&self, _session: SessionRef, _client: ClientId) -> Result<ProbeAgent, TuiError> {
        async { Ok(self.agent.clone()) }.await
    }

    async fn commands(&self) -> Result<Arc<[dal_core::CommandSpec]>, TuiError> {
        async { Ok(Arc::from(Vec::new())) }.await
    }

    async fn close(&self, _id: SessionId) -> Result<(), TuiError> {
        async {
            self.agent.log.push("host:close");
            Ok(())
        }
        .await
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
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
                .expect("fixture session id is valid"),
            name: None,
            preview: "".into(),
            workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
            updated_at: Timestamp::UNIX_EPOCH,
            created_at: Some(Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
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
