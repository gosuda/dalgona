//! A frame that never landed cannot arm approval keys.
//!
//! After an inline terminal resizes, one that answers the caret measure with
//! silence holds every paint, so an approval raised across the resize never
//! reaches the screen. Only a frame that landed with the dialog may start the
//! arming delay: keys typed while the frame is held must not answer, however
//! long the request has been open. The scripted terminal resizes into
//! silence, types `y` past twice the arming delay, then answers the measure
//! and types `y` once more after a frame lists the answer keys.

#![cfg(test)]

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_core::{
    Answer, ApprovalMode, AutoCompaction, ClientId, Command, CommandSpec, ExtStatus, Gen, Mode,
    Output, Owner, Page, PageReq, Preview, Question, Reply, Request, RequestId, Seq, SessionId,
    SessionInfo, SettingsView, Stats, ThinkingLevel, Timestamp, TreeOutline, TurnState, Update,
    UpdateKind, Usage, UsageView, View, Workspace,
};
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_tui::dialog::ARM_DELAY;
use dal_tui::term::{CursorReport, TermIo};
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};
use tokio::sync::mpsc::{Receiver, Sender};

/// The startup probe's replies from a plain terminal: no kitty keyboard.
const PROBE_REPLIES: [&[u8]; 4] = [
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

/// How long the terminal stays silent once the loop saw the resize: far past
/// the arming delay, so a clock that ignores painting has armed the dialog.
const HOLD: Duration = ARM_DELAY.saturating_mul(3);

/// The dialog's action row, which lists the answer keys only once armed.
const ARMED_HINT: &[u8] = b"y allow";

/// Fails the run instead of hanging when the script cannot finish.
const WATCHDOG: Duration = Duration::from_secs(20);

type Answers = Arc<Mutex<Vec<(RequestId, Answer)>>>;

/// Where the scripted terminal stands.
enum Phase {
    /// Feeding the startup probe its replies, one per read.
    Probe(usize),
    /// Resized into silence: typing `y` until the hold ends.
    Hold,
    /// The measure answers again: waiting for a frame that lists the keys.
    Armed,
    /// The answer key was typed; the next read ends the loop.
    Answered,
    /// The loop is told to stop.
    Quit,
}

struct Script {
    phase: Phase,
    size: (u16, u16),
    epoch: u64,
    resize_pending: bool,
    silent: bool,
    armed_hint: bool,
    /// When the delivery pump took the raised approval from the feed: the
    /// loop absorbs it within an iteration, so the hold is timed from it.
    raised_at: Option<Instant>,
    writes: usize,
    writes_at_resize: usize,
    /// `(frames written during the hold, answers the agent held at its end)`.
    at_release: Option<(usize, usize)>,
}

/// Scripted terminal: caret measures stay silent from the resize until the
/// hold ends, and every write is counted.
struct HoldIo {
    started: Instant,
    raise: Sender<Arc<Update>>,
    update: Arc<Update>,
    answers: Answers,
    delivered: Arc<AtomicBool>,
    script: Mutex<Script>,
}

impl HoldIo {
    fn new(
        raise: Sender<Arc<Update>>,
        update: Arc<Update>,
        answers: Answers,
        delivered: Arc<AtomicBool>,
    ) -> Self {
        Self {
            started: Instant::now(),
            raise,
            update,
            answers,
            delivered,
            script: Mutex::new(Script {
                phase: Phase::Probe(0),
                size: (80, 24),
                epoch: 0,
                resize_pending: false,
                silent: false,
                armed_hint: false,
                raised_at: None,
                writes: 0,
                writes_at_resize: 0,
                at_release: None,
            }),
        }
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        self.script.lock().expect("script")
    }
}

impl TermIo for HoldIo {
    fn enable_raw(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn disable_raw(&self) {}

    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok(self.script().size)
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut script = self.script();
        script.writes += 1;
        script.armed_hint |= bytes
            .windows(ARMED_HINT.len())
            .any(|window| window == ARMED_HINT);
        Ok(())
    }

    fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        if self.started.elapsed() > WATCHDOG {
            return Err(std::io::Error::other(
                "approval hold watchdog: the script never finished",
            ));
        }
        let mut script = self.script();
        match script.phase {
            Phase::Probe(next) => {
                if let Some(reply) = PROBE_REPLIES.get(next) {
                    script.phase = Phase::Probe(next + 1);
                    return Ok(reply.to_vec());
                }
                // The loop's first read: resize into a caret that never
                // answers and raise the approval across the resize.
                script.size = (100, 30);
                script.epoch += 1;
                script.resize_pending = true;
                script.silent = true;
                script.writes_at_resize = script.writes;
                self.raise
                    .try_send(Arc::clone(&self.update))
                    .expect("the one-slot feed is empty and open");
                script.phase = Phase::Hold;
                Ok(Vec::new())
            }
            Phase::Hold => {
                if script.raised_at.is_none() && self.delivered.load(Ordering::SeqCst) {
                    script.raised_at = Some(Instant::now());
                }
                let Some(waited) = script.raised_at.map(|raised| raised.elapsed()) else {
                    return Ok(Vec::new());
                };
                if waited < ARM_DELAY {
                    return Ok(Vec::new());
                }
                if waited < HOLD {
                    return Ok(b"y".to_vec());
                }
                let answered = self.answers.lock().expect("answers").len();
                script.at_release = Some((script.writes - script.writes_at_resize, answered));
                script.silent = false;
                // An answer during the hold ends the run for its assertion.
                script.phase = if answered == 0 {
                    Phase::Armed
                } else {
                    Phase::Quit
                };
                Ok(Vec::new())
            }
            Phase::Armed if script.armed_hint => {
                script.phase = Phase::Answered;
                Ok(b"y".to_vec())
            }
            Phase::Answered => {
                script.phase = Phase::Quit;
                Ok(Vec::new())
            }
            Phase::Armed | Phase::Quit => Ok(Vec::new()),
        }
    }

    fn raise_tstp(&self) {}

    fn shutdown_code(&self) -> Option<u8> {
        matches!(self.script().phase, Phase::Quit).then_some(0)
    }

    fn take_resize(&self) -> bool {
        std::mem::take(&mut self.script().resize_pending)
    }

    fn resize_epoch(&self) -> u64 {
        self.script().epoch
    }

    fn cursor_position(&self, _timeout: Duration) -> std::io::Result<CursorReport> {
        Ok(if self.script().silent {
            CursorReport::Silent
        } else {
            CursorReport::Position(24, 1)
        })
    }
}

/// The session's subscription: yields what the terminal script raises and
/// flags the moment the delivery pump took it.
struct Feed {
    updates: Receiver<Arc<Update>>,
    delivered: Arc<AtomicBool>,
}

impl TuiSubscription for Feed {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        let update = self.updates.recv().await;
        if update.is_some() {
            self.delivered.store(true, Ordering::SeqCst);
        }
        Ok(update.map(TuiDelivery::Update))
    }
}

/// Host and session in one: an idle view, a feed the terminal raises
/// approvals through, and every answer recorded.
#[derive(Clone)]
struct Asker {
    feed: Arc<Mutex<Option<Receiver<Arc<Update>>>>>,
    answers: Answers,
    delivered: Arc<AtomicBool>,
}

impl TuiAgent for Asker {
    type Subscription = Feed;

    fn session(&self) -> SessionId {
        SessionId::new_v7()
    }

    fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
        std::future::ready(Ok(idle_view()))
    }

    fn subscribe(
        &self,
        _after: Option<(Gen, Seq)>,
    ) -> impl Future<Output = Result<Feed, TuiError>> {
        let feed = self.feed.lock().expect("feed").take();
        let delivered = Arc::clone(&self.delivered);
        std::future::ready(
            feed.map(|updates| Feed { updates, delivered })
                .ok_or_else(|| {
                    TuiError::Terminal("the approval feed was already taken".to_owned())
                }),
        )
    }

    fn submit(&self, _command: Command) -> impl Future<Output = Result<Reply, TuiError>> {
        std::future::ready(Ok(Reply::Done(Output::Nothing)))
    }

    fn answer(&self, id: RequestId, answer: Answer) -> impl Future<Output = Result<(), TuiError>> {
        self.answers.lock().expect("answers").push((id, answer));
        std::future::ready(Ok(()))
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

impl TuiHost for Asker {
    type Agent = Self;

    fn open(
        &self,
        _session: SessionRef,
        _client: ClientId,
    ) -> impl Future<Output = Result<Self, TuiError>> {
        std::future::ready(Ok(self.clone()))
    }

    fn commands(&self) -> impl Future<Output = Result<Arc<[CommandSpec]>, TuiError>> {
        std::future::ready(Ok(Arc::from(Vec::new())))
    }

    fn close(&self, _id: SessionId) -> impl Future<Output = Result<(), TuiError>> {
        std::future::ready(Ok(()))
    }

    fn login(
        &self,
        _provider: &str,
        _method: Method,
        _io: LoginIo,
    ) -> impl Future<Output = Result<LoginOutcome, TuiError>> {
        std::future::ready(Err(TuiError::Terminal(
            "the hold host has no providers".to_owned(),
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

fn approval_request() -> Request {
    Request {
        id: RequestId::new_v7(),
        turn: None,
        owner: Owner::Core,
        question: Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "command".into(),
                body: "echo hi".into(),
                digest: None,
            },
            grant: None,
            call: None,
        },
        timeout: Duration::from_secs(300),
        default: Answer::Decline,
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

#[test]
fn approval_keys_wait_for_a_frame_that_showed_the_dialog() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let request = approval_request();
    let id = request.id;
    let update = Arc::new(Update {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::new(2).expect("nonzero")),
        kind: UpdateKind::RequestOpened(request),
    });
    let (raise, feed) = tokio::sync::mpsc::channel(1);
    let answers = Answers::default();
    let delivered = Arc::new(AtomicBool::new(false));
    let host = Asker {
        feed: Arc::new(Mutex::new(Some(feed))),
        answers: Arc::clone(&answers),
        delivered: Arc::clone(&delivered),
    };
    let io = HoldIo::new(raise, update, Arc::clone(&answers), delivered);

    dal_tui::run_backend(&host, &options(runtime.handle()), &io, || Ok(Vec::new()))
        .expect("the loop stops when the script quits");

    let (frames_held, answered_held) = io.script().at_release.expect("the hold ended");
    assert_eq!(
        frames_held, 0,
        "no frame may land while the measure is silent"
    );
    assert_eq!(
        answered_held, 0,
        "a key typed past the arming delay answered an approval no frame had shown"
    );
    assert_eq!(
        answers.lock().expect("answers").as_slice(),
        &[(id, Answer::Approve)],
        "the key typed once a frame listed the answer keys approves the request"
    );
}
