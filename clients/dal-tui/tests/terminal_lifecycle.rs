//! Terminal cleanup, resize recovery, and cancellation boundaries.

#![cfg(test)]

use std::collections::VecDeque;
use std::io;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_core::{
    Answer, ApprovalMode, AutoCompaction, BlobId, CallId, ClientId, Command, EntryId, EntryKind,
    EntryView, ExtStatus, FrontAction, Gen, JournalPart, Mode, Page, PageReq, Reply, RequestId,
    Seq, SessionId, SessionInfo, SessionSummary, SettingsView, Stats, ThinkingLevel, Timestamp,
    TreeOutline, TurnState, Update, UpdateKind, Usage, UsageView, View, Workspace,
};
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_tui::term::{CursorReport, TermIo};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

const PROBE_REPLIES: &[&[u8]] = &[
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

const WATCHDOG: Duration = Duration::from_secs(60);
const BLOB_TIMEOUT: Duration = Duration::from_secs(10);

const HANGING_BLOB_HEX: &str = "abababababababababababababababababababababababababababababababab";

static SERIAL: Mutex<()> = Mutex::new(());

fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn fixture_sid() -> SessionId {
    SessionId::parse("018f0f62-3b00-7000-8000-000000000001").expect("fixture session id is valid")
}

fn idle_view(sid: SessionId, entries: Vec<EntryView>) -> View {
    View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: sid,
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
            items: entries,
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
            scoped_models: Vec::new(),
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

fn options(rt: &tokio::runtime::Handle) -> TuiOptions {
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

fn pop_script(script: &Mutex<VecDeque<Vec<u8>>>) -> Option<Vec<u8>> {
    script
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .pop_front()
}

fn watch(started: Instant, label: &str) {
    assert!(
        started.elapsed() < WATCHDOG,
        "{label} watchdog: the terminal loop did not end"
    );
}

struct Silent;

impl TuiSubscription for Silent {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        async { Ok(None) }.await
    }
}

struct DropSignal {
    fired: Arc<AtomicBool>,
}

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.fired.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
struct BlobProbe {
    calls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

#[derive(Clone)]
struct Agent {
    sid: SessionId,
    entries: Vec<EntryView>,
    blob_probe: Option<BlobProbe>,
}

fn turn_cancelled_bytes() -> Vec<u8> {
    dal_tui::copy::ids::TURN_CANCELLED.as_bytes().to_vec()
}

#[derive(Clone, Default)]
struct CancelScript {
    submit: SubmitOutcome,
    deliveries: Vec<Delivery>,
    recheck: RecheckView,
}

impl CancelScript {
    const fn new(submit: SubmitOutcome, deliveries: Vec<Delivery>) -> Self {
        Self {
            submit,
            deliveries,
            recheck: RecheckView::AsOpened,
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
enum SubmitOutcome {
    #[default]
    Done,
    ResumeArmingRefuseCancel,
    ResumeArming,
}

#[derive(Clone, Default, PartialEq, Eq)]
enum RecheckView {
    #[default]
    AsOpened,
    SettlingSame,
    IdleAfterSubmit,
    RunningAfterSubmit,
    FailingAfterSubmit,
}

#[derive(Clone)]
enum Delivery {
    Ended {
        turn: dal_core::TurnId,
        stop: dal_core::Stop,
    },
    Resync,
    Transport,
    Disconnect,
}

#[derive(Clone, Default)]
struct CancelState {
    replacement_opened: Arc<AtomicBool>,
    cancel_seen: Arc<AtomicBool>,
    closes: Arc<Mutex<Vec<SessionId>>>,
    opened: Arc<Mutex<Vec<SessionRef>>>,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[derive(Clone)]
struct CancelAgent {
    sid: SessionId,
    state: CancelState,
    script: CancelScript,
    running: Option<dal_core::TurnId>,
}

impl TuiAgent for CancelAgent {
    type Subscription = CancelSubscription;

    fn session(&self) -> SessionId {
        self.sid
    }

    async fn view(&self, _page: PageReq) -> Result<View, TuiError> {
        async {
            let turn = self.running.unwrap_or_else(running_turn);
            if self.script.recheck == RecheckView::SettlingSame {
                let mut view = idle_view(self.sid, Vec::new());
                view.turn = TurnState::Settling { turn };
                return Ok(view);
            }
            if self.script.recheck != RecheckView::AsOpened
                && self.state.cancel_seen.load(Ordering::SeqCst)
            {
                return match self.script.recheck {
                    RecheckView::IdleAfterSubmit => Ok(idle_view(self.sid, Vec::new())),
                    RecheckView::RunningAfterSubmit => {
                        let mut view = idle_view(self.sid, Vec::new());
                        view.turn = TurnState::Running { turn };
                        Ok(view)
                    }
                    RecheckView::FailingAfterSubmit => Err(TuiError::Terminal(
                        "injected authoritative view failure".to_owned(),
                    )),
                    RecheckView::AsOpened | RecheckView::SettlingSame => {
                        unreachable!("phased rechecks are exhausted above")
                    }
                };
            }
            let mut view = idle_view(self.sid, Vec::new());
            if let Some(turn) = self.running {
                view.turn = TurnState::Running { turn };
            }
            Ok(view)
        }
        .await
    }

    async fn subscribe(&self, _after: Option<(Gen, Seq)>) -> Result<Self::Subscription, TuiError> {
        async {
            Ok(CancelSubscription {
                state: self.state.clone(),
                script: self.script.clone(),
                next_seq: 2,
            })
        }
        .await
    }

    async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
        async {
            if let Command::Run { name, .. } = &command
                && name.as_ref() == "resume"
            {
                return Ok(Reply::Front(FrontAction::Resume {
                    session: replacement_summary(),
                }));
            }
            if matches!(&command, Command::Cancel { .. }) {
                self.state.cancel_seen.store(true, Ordering::SeqCst);
                if self.script.submit == SubmitOutcome::ResumeArmingRefuseCancel {
                    return Err(TuiError::Terminal(
                        "session refused the cancel request: provider rejected Command::Cancel"
                            .to_owned(),
                    ));
                }
            }
            Ok(Reply::Done(dal_core::Output::Nothing))
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

fn replacement_summary() -> SessionSummary {
    SessionSummary {
        id: replacement_sid(),
        name: Some("replacement".into()),
        message_count: 1,
        updated_at: Timestamp::UNIX_EPOCH,
    }
}

fn replacement_sid() -> SessionId {
    SessionId::parse("018f0f62-3b00-7000-8000-000000000002")
        .expect("replacement session id is valid")
}

fn running_turn() -> dal_core::TurnId {
    dal_core::TurnId::new(NonZeroU64::new(7).expect("nonzero"))
}

fn other_turn() -> dal_core::TurnId {
    dal_core::TurnId::new(NonZeroU64::new(8).expect("nonzero"))
}

struct CancelSubscription {
    state: CancelState,
    script: CancelScript,
    next_seq: u64,
}

impl TuiSubscription for CancelSubscription {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        while !self.state.cancel_seen.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if self.script.deliveries.is_empty() {
            std::future::pending().await
        } else {
            let delivery = self.script.deliveries.remove(0);
            match delivery {
                Delivery::Ended { turn, stop } => {
                    let seq = NonZeroU64::new(self.next_seq).expect("fixture seq");
                    self.next_seq += 1;
                    Ok(Some(TuiDelivery::Update(Arc::new(Update {
                        r#gen: Gen::new(NonZeroU64::MIN),
                        seq: Seq::new(seq),
                        kind: UpdateKind::TurnEnded { turn, stop },
                    }))))
                }
                Delivery::Resync => Ok(Some(TuiDelivery::Resync(None))),
                Delivery::Transport => Err(TuiError::Backend(Box::new(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "subscription transport dropped the stream",
                )))),
                Delivery::Disconnect => Ok(None),
            }
        }
    }
}

#[derive(Clone)]
struct CancelHost {
    agent: CancelAgent,
    replacement: CancelAgent,
    state: CancelState,
}

impl CancelHost {
    fn new(agent: CancelAgent, replacement: CancelAgent, state: CancelState) -> Self {
        Self {
            agent,
            replacement,
            state,
        }
    }
}

impl TuiHost for CancelHost {
    type Agent = CancelAgent;

    async fn open(&self, _session: SessionRef, _client: ClientId) -> Result<CancelAgent, TuiError> {
        async { Ok(self.agent.clone()) }.await
    }

    async fn open_replacing(
        &self,
        _leaving: SessionId,
        _session: SessionRef,
        _client: ClientId,
    ) -> Result<CancelAgent, TuiError> {
        async {
            self.state.replacement_opened.store(true, Ordering::SeqCst);
            self.state
                .opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(SessionRef::Ephemeral {
                    workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
                });
            Ok(self.replacement.clone())
        }
        .await
    }

    async fn commands(&self) -> Result<Arc<[dal_core::CommandSpec]>, TuiError> {
        async { Ok(Arc::from(Vec::new())) }.await
    }

    async fn find_session(&self, reference: &str) -> Result<SessionInfo, TuiError> {
        async {
            let id = replacement_sid();
            if reference == id.to_string() || reference == "replacement" {
                return Ok(SessionInfo {
                    id,
                    name: Some("replacement".into()),
                    preview: "".into(),
                    workspace: Workspace::new(std::env::temp_dir()).expect("absolute workspace"),
                    updated_at: Timestamp::UNIX_EPOCH,
                    created_at: Some(Timestamp::UNIX_EPOCH),
                    archived: Some(false),
                    last_seq: Some(Seq::new(NonZeroU64::MIN)),
                    message_count: Some(1),
                });
            }
            Err(TuiError::Terminal(format!(
                "no session named \"{reference}\" in this workspace"
            )))
        }
        .await
    }

    async fn close(&self, id: SessionId) -> Result<(), TuiError> {
        async {
            self.state
                .closes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(id);
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
        async {
            Err(TuiError::Terminal(
                "the cancel probe host has no providers".into(),
            ))
        }
        .await
    }

    async fn logout(&self, _provider: Option<&str>) -> Result<Vec<Box<str>>, TuiError> {
        async { Ok(Vec::new()) }.await
    }

    async fn stored_credentials(&self) -> Result<Vec<StoredCredential>, TuiError> {
        async { Ok(Vec::new()) }.await
    }
}

struct CancelIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    fail_cancel_line_write: bool,
    replacement_opened: Arc<AtomicBool>,
    quit_sent: AtomicBool,
    resume_sent: AtomicBool,
    restacked: AtomicBool,
}

impl CancelIo {
    fn new(state: &CancelState, fail_cancel_line_write: bool) -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
            frames: Arc::clone(&state.frames),
            fail_cancel_line_write,
            replacement_opened: Arc::clone(&state.replacement_opened),
            quit_sent: AtomicBool::new(false),
            resume_sent: AtomicBool::new(false),
            restacked: AtomicBool::new(false),
        }
    }
}

impl TermIo for CancelIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if self.fail_cancel_line_write && has(bytes, &turn_cancelled_bytes()) {
            return Err(io::Error::other("injected terminal write failure"));
        }
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes.to_vec());
        Ok(())
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "cancel probe");
        if self.replacement_opened.load(Ordering::SeqCst)
            && !self.restacked.swap(true, Ordering::SeqCst)
        {
            let mut script = self.script.lock().unwrap_or_else(PoisonError::into_inner);
            for reply in PROBE_REPLIES {
                script.push_back(Vec::from(*reply));
            }
        }
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        if !self.replacement_opened.load(Ordering::SeqCst) {
            if !self.resume_sent.swap(true, Ordering::SeqCst) {
                return Ok(b"/resume replacement\r".to_vec());
            }
            std::thread::sleep(timeout.min(Duration::from_millis(30)));
            return Ok(Vec::new());
        }
        if !self.quit_sent.swap(true, Ordering::SeqCst) {
            return Ok(b"\x04".to_vec());
        }
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {}
}

struct CancelEofIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    fail_cancel_line_write: bool,
}

impl CancelEofIo {
    fn new(state: &CancelState, fail_cancel_line_write: bool) -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
            frames: Arc::clone(&state.frames),
            fail_cancel_line_write,
        }
    }
}

impl TermIo for CancelEofIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if self.fail_cancel_line_write && has(bytes, &turn_cancelled_bytes()) {
            return Err(io::Error::other("injected terminal write failure"));
        }
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes.to_vec());
        Ok(())
    }

    fn read(&self, _timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "cancel eof probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "terminal input closed",
        ))
    }

    fn raise_tstp(&self) {}
}

struct CancelQuitIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    quit_sent: AtomicBool,
}

impl CancelQuitIo {
    fn new(state: &CancelState) -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
            frames: Arc::clone(&state.frames),
            quit_sent: AtomicBool::new(false),
        }
    }
}

impl TermIo for CancelQuitIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes.to_vec());
        Ok(())
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "cancel quit probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        if !self.quit_sent.swap(true, Ordering::SeqCst) {
            return Ok(b"\x04".to_vec());
        }
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {}
}

fn run_cancel_quit_scenario(script: CancelScript) -> CancelOutcome {
    let runtime = cancel_probe_runtime();
    let state = CancelState::default();
    let old = CancelAgent {
        sid: fixture_sid(),
        state: state.clone(),
        script,
        running: Some(running_turn()),
    };
    let replacement = CancelAgent {
        sid: replacement_sid(),
        state: state.clone(),
        script: CancelScript::default(),
        running: None,
    };
    let host = CancelHost::new(old, replacement, state.clone());
    let io = CancelQuitIo::new(&state);
    let opts = options(runtime.handle());
    let result = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    CancelOutcome { result, state }
}

struct CancelOutcome {
    result: Result<dal_tui::TuiExit, TuiError>,
    state: CancelState,
}

fn run_cancel_scenario(script: CancelScript, fail_cancel_line_write: bool) -> CancelOutcome {
    let runtime = cancel_probe_runtime();
    let state = CancelState::default();
    let old = CancelAgent {
        sid: fixture_sid(),
        state: state.clone(),
        script,
        running: Some(running_turn()),
    };
    let replacement = CancelAgent {
        sid: replacement_sid(),
        state: state.clone(),
        script: CancelScript::default(),
        running: None,
    };
    let host = CancelHost::new(old, replacement, state.clone());
    let io = CancelIo::new(&state, fail_cancel_line_write);
    let opts = options(runtime.handle());
    let result = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    CancelOutcome { result, state }
}

fn run_cancel_eof_scenario(script: CancelScript, fail_cancel_line_write: bool) -> CancelOutcome {
    let runtime = cancel_probe_runtime();
    let state = CancelState::default();
    let agent = CancelAgent {
        sid: fixture_sid(),
        state: state.clone(),
        script,
        running: Some(running_turn()),
    };
    let host = CancelHost::new(agent.clone(), agent, state.clone());
    let io = CancelEofIo::new(&state, fail_cancel_line_write);
    let opts = options(runtime.handle());
    let result = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    CancelOutcome { result, state }
}

fn assert_two_pass_closes(state: &CancelState) {
    let closes = state
        .closes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for id in [fixture_sid(), replacement_sid()] {
        assert_eq!(
            closes.iter().filter(|closed| **closed == id).count(),
            1,
            "session {id} must close exactly once, got {closes:?}"
        );
    }
    assert_eq!(closes.len(), 2, "no third session may close: {closes:?}");
    let opened = state
        .opened
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .len();
    assert_eq!(opened, 1, "the replacement must open exactly once");
}

fn joined_frames(state: &CancelState) -> Vec<u8> {
    state
        .frames
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .concat()
}

#[test]
fn matching_cancelled_end_prints_the_cancellation_line_and_closes_both_sessions() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(
            SubmitOutcome::ResumeArming,
            vec![Delivery::Ended {
                turn: running_turn(),
                stop: dal_core::Stop::Cancelled,
            }],
        ),
        false,
    );
    assert!(
        outcome.result.is_ok(),
        "a matching Cancelled end must exit cleanly: {:?}",
        outcome.result
    );
    assert!(
        has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "the inline screen must carry the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn matching_non_cancelled_end_prints_no_cancellation_line() {
    let _serial = lock_serial();
    for stop in [dal_core::Stop::EndTurn, dal_core::Stop::Failed] {
        let outcome = run_cancel_scenario(
            CancelScript::new(
                SubmitOutcome::ResumeArming,
                vec![Delivery::Ended {
                    turn: running_turn(),
                    stop,
                }],
            ),
            false,
        );
        assert!(
            outcome.result.is_ok(),
            "a genuine non-cancelled completion is not a failure: {stop:?} {:?}",
            outcome.result
        );
        assert!(
            !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
            "stop {stop:?} must never print the cancellation line"
        );
        assert_two_pass_closes(&outcome.state);
    }
}

#[test]
fn wrong_turn_cancelled_then_matching_end_turn_completes_without_the_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(
            SubmitOutcome::ResumeArming,
            vec![
                Delivery::Ended {
                    turn: other_turn(),
                    stop: dal_core::Stop::Cancelled,
                },
                Delivery::Ended {
                    turn: running_turn(),
                    stop: dal_core::Stop::EndTurn,
                },
            ],
        ),
        false,
    );
    assert!(
        outcome.result.is_ok(),
        "a wrong-turn Cancelled followed by a matching EndTurn must complete cleanly: {:?}",
        outcome.result
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a wrong-turn cancellation must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn resync_delivery_ends_the_cancel_wait_without_a_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(SubmitOutcome::ResumeArming, vec![Delivery::Resync]),
        false,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a resync during the cancel wait must surface as a descriptive error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("cancel"),
        "the error must name the cancellation outcome, got: {message}"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a resync must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn subscription_transport_error_ends_the_cancel_wait_without_a_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(SubmitOutcome::ResumeArming, vec![Delivery::Transport]),
        false,
    );
    let Err(TuiError::Backend(error)) = outcome.result else {
        panic!(
            "a subscription transport error must retain its typed cause, got {:?}",
            outcome.result
        );
    };
    let cause = error
        .downcast_ref::<io::Error>()
        .expect("the original transport error must remain inspectable");
    assert_eq!(cause.kind(), io::ErrorKind::ConnectionReset);
    assert_eq!(
        cause.to_string(),
        "subscription transport dropped the stream"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a transport error must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn subscription_disconnect_ends_the_cancel_wait_without_a_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(SubmitOutcome::ResumeArming, vec![Delivery::Disconnect]),
        false,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a subscription disconnect must surface as a descriptive error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("cancel"),
        "the error must name the cancellation outcome, got: {message}"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a disconnect must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn silent_subscription_reports_a_descriptive_timeout_error() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(SubmitOutcome::ResumeArming, vec![]),
        false,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a silent subscription (timeout) must surface as a descriptive error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("cancel"),
        "the error must name the cancellation outcome, got: {message}"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a timed-out wait must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn fullscreen_cancelled_end_closes_both_sessions_without_the_inline_line() {
    let _serial = lock_serial();
    let runtime = cancel_probe_runtime();
    let state = CancelState::default();
    let old = CancelAgent {
        sid: fixture_sid(),
        state: state.clone(),
        script: CancelScript::new(
            SubmitOutcome::ResumeArming,
            vec![Delivery::Ended {
                turn: running_turn(),
                stop: dal_core::Stop::Cancelled,
            }],
        ),
        running: Some(running_turn()),
    };
    let replacement = CancelAgent {
        sid: replacement_sid(),
        state: state.clone(),
        script: CancelScript::default(),
        running: None,
    };
    let host = CancelHost::new(old, replacement, state.clone());
    let io = CancelIo::new(&state, false);
    let mut opts = options(runtime.handle());
    opts.screen = Screen::Fullscreen;
    let result = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    assert!(
        result.is_ok(),
        "a fullscreen Cancelled end must exit cleanly: {result:?}"
    );
    assert!(
        !has(&joined_frames(&state), &turn_cancelled_bytes()),
        "fullscreen prints no inline cancellation line even on true"
    );
    assert_two_pass_closes(&state);
}

#[test]
fn refused_cancel_submit_reports_a_descriptive_error_and_closes_both_sessions() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(SubmitOutcome::ResumeArmingRefuseCancel, vec![]),
        false,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a refused Cancel submit must surface as a descriptive error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("cancel"),
        "the error must name the refused cancel, got: {message}"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a refused cancel never cancels anything, so no cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

const REFUSED_CANCEL: &str =
    "session refused the cancel request: provider rejected Command::Cancel";

fn rechecked_script(recheck: RecheckView) -> CancelScript {
    CancelScript {
        submit: SubmitOutcome::ResumeArmingRefuseCancel,
        deliveries: Vec::new(),
        recheck,
    }
}

fn assert_single_pass_close(state: &CancelState) {
    let closes = state
        .closes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        closes,
        vec![fixture_sid()],
        "the owned session must close exactly once, got {closes:?}"
    );
    let opened = state
        .opened
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        opened.len(),
        0,
        "no replacement may open on the plain exit path"
    );
}

#[test]
fn settling_turn_resume_completes_without_error_or_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(rechecked_script(RecheckView::SettlingSame), false);
    assert!(
        outcome.result.is_ok(),
        "a cancel rejected for a durably ended (Settling) turn must resume cleanly: {:?}",
        outcome.result
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "an already-finished turn must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn settling_turn_quit_completes_without_error_or_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_quit_scenario(rechecked_script(RecheckView::SettlingSame));
    assert!(
        outcome.result.is_ok(),
        "a quit with a durably ended (Settling) turn must exit cleanly: {:?}",
        outcome.result
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "an already-finished turn must never print the cancellation line"
    );
    assert_single_pass_close(&outcome.state);
}

#[test]
fn settling_turn_eof_keeps_eof_precedence_and_submits_no_cancel() {
    let _serial = lock_serial();
    let outcome = run_cancel_eof_scenario(rechecked_script(RecheckView::SettlingSame), false);
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "the EOF pass must fail with its original error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("terminal input closed"),
        "the original EOF error must win, got: {message}"
    );
    assert!(
        !outcome.state.cancel_seen.load(Ordering::SeqCst),
        "a Settling fallback arms no cancellable turn, so no cancel may be submitted"
    );
    assert_single_pass_close(&outcome.state);
}

#[test]
fn finished_turn_before_rejected_submit_resumes_without_error_or_line() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(rechecked_script(RecheckView::IdleAfterSubmit), false);
    assert!(
        outcome.result.is_ok(),
        "a turn that finished before its rejected cancel submit must resume cleanly: {:?}",
        outcome.result
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "an already-finished turn must never print the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn genuine_refusal_while_same_turn_runs_keeps_the_original_error_verbatim() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(rechecked_script(RecheckView::RunningAfterSubmit), false);
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a refusal while the same turn still runs must stay an error, got {:?}",
            outcome.result
        );
    };
    assert_eq!(
        message, REFUSED_CANCEL,
        "the original submit error must survive verbatim"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a refused cancel never cancels anything, so no cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn view_read_failure_keeps_the_original_submit_error() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(rechecked_script(RecheckView::FailingAfterSubmit), false);
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a failed authoritative re-read must stay an error, got {:?}",
            outcome.result
        );
    };
    assert_eq!(
        message, REFUSED_CANCEL,
        "a failed re-read must not mask the original submit error"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "a refused cancel never cancels anything, so no cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn write_failure_while_printing_cancellation_reports_the_error_and_closes_both_sessions() {
    let _serial = lock_serial();
    let outcome = run_cancel_scenario(
        CancelScript::new(
            SubmitOutcome::ResumeArming,
            vec![Delivery::Ended {
                turn: running_turn(),
                stop: dal_core::Stop::Cancelled,
            }],
        ),
        true,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "a terminal-write failure while printing the cancellation must surface as an error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("injected terminal write failure"),
        "the error must name the write failure, got: {message}"
    );
    assert!(
        !has(&joined_frames(&outcome.state), &turn_cancelled_bytes()),
        "the failed write must not have committed the cancellation line"
    );
    assert_two_pass_closes(&outcome.state);
}

#[test]
fn eof_write_failure_reports_the_original_eof_error_and_closes_the_session() {
    let _serial = lock_serial();
    let outcome = run_cancel_eof_scenario(
        CancelScript::new(
            SubmitOutcome::ResumeArming,
            vec![Delivery::Ended {
                turn: running_turn(),
                stop: dal_core::Stop::Cancelled,
            }],
        ),
        true,
    );
    let Err(TuiError::Terminal(message)) = outcome.result else {
        panic!(
            "the EOF pass must fail with its original error, got {:?}",
            outcome.result
        );
    };
    assert!(
        message.contains("terminal input closed"),
        "the original EOF error must win, got: {message}"
    );
    let closes = outcome
        .state
        .closes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        closes,
        vec![fixture_sid()],
        "the owned session must close exactly once, got {closes:?}"
    );
    assert!(
        outcome
            .state
            .opened
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "no replacement may open on the plain exit path"
    );
}

impl Agent {
    fn idle(sid: SessionId) -> Self {
        Self {
            sid,
            entries: Vec::new(),
            blob_probe: None,
        }
    }

    fn tool_with_hanging_blob(sid: SessionId, probe: BlobProbe) -> Self {
        let entry = EntryView {
            id: EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: EntryKind::ToolResult {
                call: CallId::new("call-hang"),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::TextBlob {
                    blob: HANGING_BLOB_HEX.into(),
                    bytes: 3,
                }],
                changes: Vec::new(),
                elapsed_ms: None,
            },
        };
        Self {
            sid,
            entries: vec![entry],
            blob_probe: Some(probe),
        }
    }
}

impl TuiAgent for Agent {
    type Subscription = Silent;

    fn session(&self) -> SessionId {
        self.sid
    }

    async fn view(&self, _page: PageReq) -> Result<View, TuiError> {
        async { Ok(idle_view(self.sid, self.entries.clone())) }.await
    }

    async fn subscribe(&self, _after: Option<(Gen, Seq)>) -> Result<Silent, TuiError> {
        async { Ok(Silent) }.await
    }

    async fn submit(&self, _command: Command) -> Result<Reply, TuiError> {
        async { Ok(Reply::Done(dal_core::Output::Nothing)) }.await
    }

    async fn answer(&self, _id: RequestId, _answer: Answer) -> Result<(), TuiError> {
        async { Ok(()) }.await
    }

    async fn blob(&self, id: BlobId) -> Result<Arc<[u8]>, TuiError> {
        let _ = id;
        match &self.blob_probe {
            Some(probe) => {
                probe.calls.fetch_add(1, Ordering::SeqCst);
                let _signal = DropSignal {
                    fired: Arc::clone(&probe.dropped),
                };
                let () = std::future::pending().await;
                unreachable!("the hanging blob read never resolves; teardown must cut it");
            }
            None => Err(TuiError::Terminal(
                "dal-tui: this host cannot read session blobs".to_owned(),
            )),
        }
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

#[derive(Clone)]
struct Host {
    closes: Arc<AtomicUsize>,
    agent: Agent,
}

impl Host {
    fn with_agent(agent: Agent) -> (Self, Arc<AtomicUsize>) {
        let closes = Arc::new(AtomicUsize::new(0));
        (
            Self {
                closes: Arc::clone(&closes),
                agent,
            },
            closes,
        )
    }
}

impl TuiHost for Host {
    type Agent = Agent;

    async fn open(&self, _session: SessionRef, _client: ClientId) -> Result<Agent, TuiError> {
        async { Ok(self.agent.clone()) }.await
    }

    async fn commands(&self) -> Result<Arc<[dal_core::CommandSpec]>, TuiError> {
        async { Ok(Arc::from(Vec::new())) }.await
    }

    async fn close(&self, _id: SessionId) -> Result<(), TuiError> {
        async {
            self.closes.fetch_add(1, Ordering::SeqCst);
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

struct EofIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
}

impl EofIo {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
        }
    }
}

impl TermIo for EofIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read(&self, _timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "closed-stdin probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "terminal input closed",
        ))
    }

    fn raise_tstp(&self) {}
}

struct EpochIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    epoch: Mutex<u64>,
    resize_pending: Mutex<bool>,
    reports: Mutex<VecDeque<CursorReport>>,
    cursor_calls: AtomicUsize,
    fail_size_next: AtomicBool,
    bumped: AtomicBool,
    quit_sent: AtomicBool,
    frames: Mutex<Vec<Vec<u8>>>,
    bump_frame_index: AtomicUsize,
}

impl EpochIo {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
            epoch: Mutex::new(0),
            resize_pending: Mutex::new(false),
            reports: Mutex::new(VecDeque::new()),
            cursor_calls: AtomicUsize::new(0),
            fail_size_next: AtomicBool::new(false),
            bumped: AtomicBool::new(false),
            quit_sent: AtomicBool::new(false),
            frames: Mutex::new(Vec::new()),
            bump_frame_index: AtomicUsize::new(0),
        }
    }

    fn bump_epoch_same_size(&self) {
        *self.epoch.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        *self
            .resize_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(CursorReport::Position(10, 1));
        self.fail_size_next.store(true, Ordering::SeqCst);
        let frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        self.bump_frame_index.store(frames.len(), Ordering::SeqCst);
    }
}

impl TermIo for EpochIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        if self.fail_size_next.swap(false, Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "injected size error",
            ));
        }
        Ok((80, 24))
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes.to_vec());
        Ok(())
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "epoch probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        if !self.bumped.swap(true, Ordering::SeqCst) {
            self.bump_epoch_same_size();
            return Ok(Vec::new());
        }
        if !self.quit_sent.swap(true, Ordering::SeqCst) {
            return Ok(b"\x04".to_vec());
        }
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {}

    fn take_resize(&self) -> bool {
        std::mem::replace(
            &mut *self
                .resize_pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            false,
        )
    }

    fn resize_epoch(&self) -> u64 {
        *self.epoch.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cursor_position(&self, _timeout: Duration) -> io::Result<CursorReport> {
        self.cursor_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(CursorReport::Position(24, 1)))
    }
}

struct BlobIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    probe: BlobProbe,
    opened: AtomicBool,
    collapsed: AtomicBool,
    quit_sent: AtomicBool,
}

impl BlobIo {
    fn new(probe: &BlobProbe) -> Self {
        Self {
            started: Instant::now(),
            script: Mutex::new(PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect()),
            probe: probe.clone(),
            opened: AtomicBool::new(false),
            collapsed: AtomicBool::new(false),
            quit_sent: AtomicBool::new(false),
        }
    }
}

impl TermIo for BlobIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "blob probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        if !self.opened.swap(true, Ordering::SeqCst) {
            return Ok(vec![0x0f]);
        }
        if self.probe.calls.load(Ordering::SeqCst) == 0 {
            assert!(
                self.started.elapsed() <= Duration::from_secs(5),
                "blob probe: the expand view never started its blob read"
            );
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            return Ok(Vec::new());
        }
        if !self.collapsed.swap(true, Ordering::SeqCst) {
            return Ok(vec![0x0f]);
        }
        if !self.probe.dropped.load(Ordering::SeqCst) {
            assert!(
                self.started.elapsed() <= Duration::from_secs(5),
                "blob probe: closing the view did not cut the in-flight read"
            );
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            return Ok(Vec::new());
        }
        if !self.quit_sent.swap(true, Ordering::SeqCst) {
            return Ok(b"\x04".to_vec());
        }
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {}
}

struct SuspendIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    raises: AtomicUsize,
}

impl SuspendIo {
    fn new() -> Self {
        let mut script: VecDeque<Vec<u8>> = PROBE_REPLIES.iter().map(|b| Vec::from(*b)).collect();
        script.push_back(vec![0x1a]);
        script.push_back(vec![0x04]);
        Self {
            started: Instant::now(),
            script: Mutex::new(script),
            raises: AtomicUsize::new(0),
        }
    }
}

impl TermIo for SuspendIo {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }

    fn write(&self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        watch(self.started, "suspend probe");
        if let Some(next) = pop_script(&self.script) {
            return Ok(next);
        }
        std::thread::sleep(timeout.min(Duration::from_millis(30)));
        Ok(Vec::new())
    }

    fn raise_tstp(&self) {
        self.raises.fetch_add(1, Ordering::SeqCst);
    }
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime")
}

fn cancel_probe_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("cancel probe runtime")
}

#[test]
fn closed_stdin_surfaces_a_descriptive_terminal_error_and_closes_the_session() {
    let _serial = lock_serial();
    let runtime = current_thread_runtime();
    let sid = fixture_sid();
    let (host, closes) = Host::with_agent(Agent::idle(sid));
    let opts = options(runtime.handle());
    let io = EofIo::new();
    let outcome = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    let Err(TuiError::Terminal(message)) = outcome else {
        panic!("closed stdin must return Err(Terminal), got {outcome:?}");
    };
    assert!(
        message.contains("terminal input closed"),
        "the error must name the closed-input cause, got: {message}"
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "the error path must still close the owned session exactly once"
    );
}

#[test]
fn same_size_epoch_bump_relocates_and_transient_size_error_falls_back() {
    let _serial = lock_serial();
    let runtime = current_thread_runtime();
    let sid = fixture_sid();
    let (host, closes) = Host::with_agent(Agent::idle(sid));
    let opts = options(runtime.handle());
    let io = EpochIo::new();
    let outcome = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    assert!(
        outcome.is_ok(),
        "epoch bump plus one post-startup size error must not break the loop: {outcome:?}"
    );
    assert!(
        io.cursor_calls.load(Ordering::SeqCst) != 0,
        "the same-size epoch bump must trigger a caret re-measure"
    );
    let frames = io.frames.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(!frames.is_empty(), "the loop must paint at least one frame");
    let bump_index = io.bump_frame_index.load(Ordering::SeqCst);
    assert!(
        frames[bump_index..]
            .iter()
            .any(|frame| has(frame, b"\x1b[10;1H")),
        "a post-bump frame must address the relocated origin on the tenth row"
    );
    for (index, frame) in frames.iter().enumerate() {
        assert!(!has(frame, b"\x1b[J"), "frame {index} emitted ED0");
        assert!(!has(frame, b"\x1b[2J"), "frame {index} emitted ED2");
        assert!(!has(frame, b"\x1b[3J"), "frame {index} cleared scrollback");
    }
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "the pass must close its session exactly once"
    );
}

#[test]
fn hanging_blob_read_is_cut_on_view_close_without_stalling_quit() {
    let _serial = lock_serial();
    let started = Instant::now();
    let runtime = current_thread_runtime();
    let sid = fixture_sid();
    let probe = BlobProbe::default();
    let (host, closes) = Host::with_agent(Agent::tool_with_hanging_blob(sid, probe.clone()));
    let opts = options(runtime.handle());
    let io = BlobIo::new(&probe);
    let outcome = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    assert!(
        outcome.is_ok(),
        "closing the expand view must not stall quit: {outcome:?}"
    );
    assert!(
        probe.calls.load(Ordering::SeqCst) != 0,
        "the expand view must have started its blob read"
    );
    assert!(
        probe.dropped.load(Ordering::SeqCst),
        "closing the view must drop the in-flight blob future before quit"
    );
    assert!(
        started.elapsed() < BLOB_TIMEOUT,
        "teardown must cut the hanging read before its 10 s timeout (took {:?})",
        started.elapsed()
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "the pass must close its session exactly once"
    );
}

#[test]
fn immediate_raise_resumes_and_finishes_within_deadline() {
    let _serial = lock_serial();
    let started = Instant::now();
    let runtime = current_thread_runtime();
    let sid = fixture_sid();
    let (host, closes) = Host::with_agent(Agent::idle(sid));
    let opts = options(runtime.handle());
    let io = SuspendIo::new();
    let outcome = dal_tui::run_backend(&host, &opts, &io, || Ok(Vec::new()));
    assert!(
        outcome.is_ok(),
        "an immediately-returning raise must still resume and finish: {outcome:?}"
    );
    assert_eq!(
        io.raises.load(Ordering::SeqCst),
        1,
        "suspend must have raised exactly once"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the pass must finish within its deadline (took {:?})",
        started.elapsed()
    );
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "the pass must close its session exactly once"
    );
}
