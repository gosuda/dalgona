//! Panic-hook ownership: a panic reaches the process's original hook exactly
//! once while terminal passes run, unwind, overlap, and end, and no pass
//! changes the process hook while a panic unwinds it.
//!
//! The hook is process-global, so both tests run under one lock and share one
//! stand-in for the original hook, installed once before any pass starts.

#![cfg(test)]

use std::collections::VecDeque;
use std::io;
use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, PanicHookInfo};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::thread::ScopedJoinHandle;
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
use dal_core::{
    Answer, ApprovalMode, AutoCompaction, ClientId, Command, ExtStatus, Gen, Mode, Page, PageReq,
    Reply, RequestId, Seq, SessionId, SessionInfo, SettingsView, Stats, ThinkingLevel, Timestamp,
    TreeOutline, TurnState, Usage, UsageView, View, Workspace,
};
use dal_tui::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use dal_tui::term::TermIo;
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

const PROBE_REPLIES: &[&[u8]] = &[
    b"\x1b[?2026;2$y",
    b"\x1b[?2027;2$y",
    b"\x1b]11;rgb:0000/0000/0000\x07",
    b"\x1b[?1;2c",
];

/// A pass that has not ended by then is stuck; the read panics instead of
/// leaving the test to hang.
const WATCHDOG: Duration = Duration::from_secs(30);

const LOOP_PANIC: &str = "panic-hook probe: the terminal read failed";
const OUTSIDE_PANIC: &str = "panic-hook probe: a panic outside any pass";
const AFTER_PANIC: &str = "panic-hook probe: a panic after a finished pass";
const ATTACH_ORDER_PANIC: &str = "panic-hook probe: after passes ended in attach order";
const REVERSE_ORDER_PANIC: &str = "panic-hook probe: after passes ended in reverse order";

static SERIAL: Mutex<()> = Mutex::new(());
static ORIGINAL: Once = Once::new();

/// Every panic the original hook was handed since the current test began.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn seen() -> Vec<String> {
    SEEN.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// Takes the file-wide lock, puts the stand-in for the process's original hook
/// in place (once; it records each panic it is handed, then reports it through
/// the hook the test harness installed), and starts a fresh record.
fn begin() -> MutexGuard<'static, ()> {
    let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    ORIGINAL.call_once(|| {
        let reporting = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            SEEN.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(message(info));
            reporting(info);
        }));
    });
    SEEN.lock().unwrap_or_else(PoisonError::into_inner).clear();
    serial
}

/// How a scripted terminal behaves once its script is spent.
enum Ending {
    /// The next read panics, unwinding the pass that made it.
    Panic,
    /// Reads stay idle until `release` is set, then Ctrl+D ends the pass.
    Hold { release: Arc<AtomicBool> },
}

/// A terminal that answers the startup probe and plays `keys`.
struct ScriptedIo {
    started: Instant,
    script: Mutex<VecDeque<Vec<u8>>>,
    ending: Ending,
    /// Counts the passes whose main loop is reading, so a test can overlap them.
    parked: Arc<AtomicUsize>,
    announced: AtomicBool,
}

impl ScriptedIo {
    fn new(keys: &[&[u8]], ending: Ending, parked: Arc<AtomicUsize>) -> Self {
        let script = PROBE_REPLIES
            .iter()
            .chain(keys.iter())
            .map(|bytes| Vec::from(*bytes))
            .collect();
        Self {
            started: Instant::now(),
            script: Mutex::new(script),
            ending,
            parked,
            announced: AtomicBool::new(false),
        }
    }
}

impl TermIo for ScriptedIo {
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
        assert!(
            self.started.elapsed() < WATCHDOG,
            "panic-hook probe watchdog: the pass did not end"
        );
        let next = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        if let Some(bytes) = next {
            return Ok(bytes);
        }
        if !self.announced.swap(true, Ordering::SeqCst) {
            self.parked.fetch_add(1, Ordering::SeqCst);
        }
        match &self.ending {
            Ending::Panic => std::panic::panic_any(LOOP_PANIC),
            Ending::Hold { release } if release.load(Ordering::SeqCst) => Ok(b"\x04".to_vec()),
            Ending::Hold { .. } => {
                std::thread::sleep(timeout.min(Duration::from_millis(30)));
                Ok(Vec::new())
            }
        }
    }

    fn raise_tstp(&self) {}
}

struct Silent;

impl TuiSubscription for Silent {
    async fn next(&mut self) -> Result<Option<TuiDelivery>, TuiError> {
        async { Ok(None) }.await
    }
}

#[derive(Clone)]
struct Agent;

impl TuiAgent for Agent {
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

    async fn submit(&self, _command: Command) -> Result<Reply, TuiError> {
        async { Ok(Reply::Done(dal_core::Output::Nothing)) }.await
    }

    async fn answer(&self, _id: RequestId, _answer: Answer) -> Result<(), TuiError> {
        async { Ok(()) }.await
    }

    fn ext_status(&self) -> Vec<ExtStatus> {
        Vec::new()
    }
}

#[derive(Clone)]
struct Host;

impl TuiHost for Host {
    type Agent = Agent;

    async fn open(&self, _session: SessionRef, _client: ClientId) -> Result<Agent, TuiError> {
        async { Ok(Agent) }.await
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

fn run_pass(io: &ScriptedIo) -> Result<dal_tui::TuiExit, TuiError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    let opts = options(runtime.handle());
    dal_tui::run_backend(&Host, &opts, io, || Ok(Vec::new()))
}

/// A pass whose terminal read panics, unwinding through the pass.
fn unwinding_io() -> ScriptedIo {
    ScriptedIo::new(&[], Ending::Panic, Arc::new(AtomicUsize::new(0)))
}

/// Panics on this thread and returns once the panic is caught.
fn panic_outside_any_pass(label: &'static str) {
    let outcome = std::panic::catch_unwind(|| {
        std::panic::panic_any(label);
    });
    assert!(outcome.is_err(), "the probe panic must unwind");
}

#[test]
fn a_pass_unwound_by_a_panic_reaches_the_original_hook_once_and_never_aborts() {
    let _serial = begin();

    // A panic in the loop thread unwinds the pass, so the pass's guard drops on
    // a panicking thread. Touching the process hook there panics inside a
    // destructor during unwinding, which aborts the process before this
    // assertion can run.
    let io = unwinding_io();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| run_pass(&io)));
    let Err(payload) = outcome else {
        panic!("the scripted read panics, so the pass cannot return");
    };
    assert_eq!(payload.downcast_ref::<&str>().copied(), Some(LOOP_PANIC));
    assert_eq!(
        seen(),
        [LOOP_PANIC],
        "the original hook reports the loop's panic exactly once"
    );

    // The unwound pass left the hook chain as it found it.
    panic_outside_any_pass(OUTSIDE_PANIC);
    assert_eq!(
        seen(),
        [LOOP_PANIC, OUTSIDE_PANIC],
        "a later panic reaches the original hook once, with no stale hook ahead of it"
    );

    // A pass that ends by Ctrl+D leaves the hook chain as it found it too.
    let clean = ScriptedIo::new(&[b"\x04"], Ending::Panic, Arc::new(AtomicUsize::new(0)));
    run_pass(&clean).expect("Ctrl+D on an empty composer ends the pass");
    panic_outside_any_pass(AFTER_PANIC);
    assert_eq!(
        seen(),
        [LOOP_PANIC, OUTSIDE_PANIC, AFTER_PANIC],
        "a panic after a finished pass reaches the original hook once"
    );

    // A second pass unwinds the same way over what the first ones left.
    let again = unwinding_io();
    let second = std::panic::catch_unwind(AssertUnwindSafe(|| run_pass(&again)));
    assert!(second.is_err(), "the second scripted read panics too");
    assert_eq!(
        seen(),
        [LOOP_PANIC, OUTSIDE_PANIC, AFTER_PANIC, LOOP_PANIC],
        "the second unwind reaches the original hook once"
    );
}

/// Two passes alive together; the one that attached first ends first.
fn passes_ending_in_attach_order() {
    overlapping_passes([0, 1]);
}

/// Two passes alive together; the one that attached last ends first.
fn passes_ending_in_reverse_order() {
    overlapping_passes([1, 0]);
}

/// Attaches pass 0, then pass 1 on another thread, ends them in `end_order`
/// (pass indices) while both are attached, and joins both.
fn overlapping_passes(end_order: [usize; 2]) {
    let parked = Arc::new(AtomicUsize::new(0));
    let release = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    let ios = [
        ScriptedIo::new(
            &[],
            Ending::Hold {
                release: Arc::clone(&release[0]),
            },
            Arc::clone(&parked),
        ),
        ScriptedIo::new(
            &[],
            Ending::Hold {
                release: Arc::clone(&release[1]),
            },
            Arc::clone(&parked),
        ),
    ];
    std::thread::scope(|scope| {
        let wait_parked = |count: usize| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while parked.load(Ordering::SeqCst) < count && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            parked.load(Ordering::SeqCst) >= count
        };
        // The second pass starts only once the first is attached, so which
        // pass attached first is fixed.
        let pass_0 = scope.spawn(|| run_pass(&ios[0]));
        let first_attached = wait_parked(1);
        let pass_1 = scope.spawn(|| run_pass(&ios[1]));
        let attached = first_attached && wait_parked(2);
        if !attached {
            // Let both end so the scope can join them before the test fails.
            for flag in &release {
                flag.store(true, Ordering::SeqCst);
            }
        }
        let mut passes = [Some(pass_0), Some(pass_1)];
        let [early, late] = end_order;
        // The early pass ends while the late one is still attached.
        release[early].store(true, Ordering::SeqCst);
        let early_outcome = passes[early].take().map(ScopedJoinHandle::join);
        release[late].store(true, Ordering::SeqCst);
        let late_outcome = passes[late].take().map(ScopedJoinHandle::join);
        assert!(attached, "both passes must be attached before either ends");
        for outcome in [early_outcome, late_outcome] {
            outcome
                .expect("each pass is joined once")
                .expect("a pass thread does not panic")
                .expect("a pass ends by Ctrl+D");
        }
    });
}

#[test]
fn passes_alive_together_leave_the_original_hook_in_place() {
    let _serial = begin();

    // A hook swapped in on attach and back on detach loses the original when
    // the pass that attached first ends first: the later swap-back installs the
    // earlier pass's hook, whose own record of the original was already taken.
    passes_ending_in_attach_order();
    panic_outside_any_pass(ATTACH_ORDER_PANIC);
    assert_eq!(
        seen(),
        [ATTACH_ORDER_PANIC],
        "after passes ended in attach order a panic still reaches the original hook once"
    );

    passes_ending_in_reverse_order();
    panic_outside_any_pass(REVERSE_ORDER_PANIC);
    assert_eq!(
        seen(),
        [ATTACH_ORDER_PANIC, REVERSE_ORDER_PANIC],
        "after passes ended in reverse order a panic still reaches the original hook once"
    );
}
