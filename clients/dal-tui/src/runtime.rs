use crate::theme::ResolvedTheme;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{Method, SecretString};
use dal_core::{
    Answer, BlobId, CancelScope, Chooser, ClientId, Command, Expect, FrontAction, Output, PageReq,
    Reply, RequestId, ResolutionCause, Stop, TurnId, TurnState, UpdateKind,
};

use crate::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use crate::composer::Composer;
use crate::dialog::DialogUi;
use crate::help::HelpUi;
use crate::keys::{Action, InputEvent, KeyDecoder, Owner, resolve_in};
use crate::live::{BlobRead, Live};
use crate::picker::{ModelOption, PickerAction, PickerUi};
use crate::render::{FrameInput, entry_rows_timed};
use crate::screen::driver::Painter;
use crate::screen::fullscreen::SelectionStep;
use crate::signin::{self, KeyAction, SignIn};
use crate::term::{ReplyParser, TermIo, TermState, restore, startup_probe};
use crate::transcript::Transcript;
use crate::{TuiError, TuiExit, TuiOptions};

const RESOLUTION_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Renders the outcome note for a request this client showed.
///
/// The wording follows the resolution cause alone: a missing cause (an old
/// journal) or one this build does not know reads as a neutral "resolved"
/// note and is never guessed from the answering client, the answer or time.
fn resolution_notice(
    cause: Option<ResolutionCause>,
    by: &ClientId,
    answer: &Answer,
    title: &str,
) -> String {
    use crate::copy::ids;

    let template = match cause {
        Some(ResolutionCause::Answered) => ids::REQUEST_RESOLVED_BY,
        Some(ResolutionCause::TimedOut) => ids::REQUEST_TIMED_OUT,
        Some(ResolutionCause::Cancelled) => ids::REQUEST_CANCELLED,
        Some(ResolutionCause::Withdrawn) => ids::REQUEST_WITHDRAWN,
        Some(ResolutionCause::NoController) => ids::REQUEST_NO_CONTROLLER,
        None | Some(_) => ids::REQUEST_RESOLVED,
    };
    crate::copy::render(
        template,
        &[
            ("client", by.as_str()),
            ("answer", &crate::copy::answer_word(answer)),
            ("title", &crate::width::escape(title)),
        ],
        1,
    )
}

/// Local composer and exit state; transport to `Agent::submit` lands with `Host::open`.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent session flags; a state machine hides the loop wiring"
)]
struct Session {
    composer: Composer,
    /// The draft-discard question, while it owns the composer and hint rows.
    /// Focus starts on the conservative `Keep` verb; a blind Enter never
    /// throws the draft away.
    dialog: Option<crate::dialog::DraftConfirm>,
    quit: bool,
    overlay: bool,
    /// Fullscreen transcript viewport: follow, frozen scroll window, search.
    viewport: crate::screen::fullscreen::Viewport,
    active_turn: Option<TurnId>,
    pending_commands: Vec<Command>,
    /// Reply text a command asked the terminal to copy, in request order.
    pending_copies: Vec<Box<str>>,
    pending_answers: Vec<(RequestId, Answer)>,
    pending_diagram_settings: Vec<(bool, dal_core::command::Save)>,
    /// Set when the failed model picker saw `r`; the next tick reruns the
    /// real model source and rebuilds the picker, query/filter intact.
    pending_model_retry: bool,
    command_seq: u64,
    commands: std::sync::Arc<[dal_core::CommandSpec]>,
    popup: Vec<String>,
    candidates: Vec<crate::popup::Candidate>,
    picker: Option<PickerUi>,
    /// The F1 key-table overlay, while it owns the composer and hint rows.
    help: Option<HelpUi>,
    signin: Option<SignIn>,
    front: Vec<FrontRequest>,
    cancelling: Option<TurnId>,
    cancel_settled: bool,
    cancel_deadline: Option<std::time::Instant>,
    /// The one `Run` command still executing off this thread.
    inflight: Option<InflightRun>,
    /// Set when Ctrl+Z asked the loop to park the terminal and raise SIGTSTP.
    suspend: bool,
    /// The blob read worker serving the open expand view, for structured
    /// cancellation and a clean join at teardown.
    blob_load: Option<BlobReadWork>,
}

/// A terminal-only request that needs the host or agent; the loop runs it
/// after the input pass.
#[derive(Debug)]
enum FrontRequest {
    /// Sign in to a provider.
    Login(Box<str>),
    /// Start the API-key login with the typed key.
    StartKey {
        provider: Box<str>,
        key: SecretString,
    },
    /// Open the logout picker with the stored credentials.
    LogoutPicker(Box<str>),
    /// Remove one provider's credential.
    Logout(Box<str>),
    /// Remove every credential after the confirmation.
    LogoutAll,
    /// Start the expand view's blob reads on a worker: the `Weak` sink dies
    /// with the view, so closing the card cancels every remaining read.
    LoadBlobs(Weak<Mutex<HashMap<BlobId, BlobRead>>>),
}

struct CommandContext<H, M> {
    specs: std::sync::Arc<[dal_core::CommandSpec]>,
    model_source: M,
    host: H,
}

impl Session {
    fn submit_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            self.composer.take();
            return;
        }
        match dal_core::command::classify(line) {
            dal_core::command::Classify::Command { name, args } => {
                self.pending_commands.push(Command::Run {
                    name,
                    args,
                    expected: None,
                });
            }
            dal_core::command::Classify::Text => {
                let content = vec![dal_core::Part::Text {
                    text: line.to_owned().into_boxed_str(),
                }];
                // Text typed while a turn runs steers that turn at its next
                // safe point; a prompt would be refused as a turn mismatch.
                self.pending_commands.push(match self.active_turn {
                    Some(turn) => Command::Steer { turn, content },
                    None => Command::Prompt {
                        expect: Expect::Idle,
                        content,
                    },
                });
            }
        }
        self.composer.take();
        self.update_popup();
    }

    fn update_popup(&mut self) {
        self.popup.clear();
        self.candidates.clear();
        match crate::popup::complete_draft(&self.commands, self.composer.text()) {
            crate::popup::DraftCompletion::Names(candidates) => {
                self.candidates = candidates.into_iter().take(5).collect();
                self.popup.extend(self.candidates.iter().map(|item| {
                    format!(
                        "{}{} · {}",
                        item.lane.sigil(),
                        item.name,
                        crate::width::escape(&item.summary)
                    )
                }));
            }
            crate::popup::DraftCompletion::Args { name, .. } => {
                if let Some(spec) = self.commands.iter().find(|spec| spec.name.as_str() == name)
                    && let Some(hint) = spec.args_hint.as_deref()
                {
                    self.popup
                        .push(format!("/{name} · {}", crate::width::escape(hint)));
                }
            }
        }
    }

    /// The discard-draft dialog resolved to `Discard`: the draft goes and
    /// the loop quits. `Keep` answers leave the draft and session alone.
    fn discard_draft(&mut self) {
        self.quit = true;
        self.dialog = None;
        self.composer.clear();
    }

    fn interrupt(&mut self) {
        if let Some(turn) = self.active_turn {
            self.pending_commands.push(Command::Cancel {
                scope: CancelScope::Turn(turn),
            });
        }
    }

    /// Ctrl+C: stop a running sign-in, close a finished overlay, picker, expand
    /// view, or the key help, copy the fullscreen transcript selection, else
    /// interrupt the turn.
    fn interrupt_or_dismiss(&mut self, live: &mut Live, transcript: &Transcript) {
        if let Some(signin) = self.signin.as_mut() {
            if signin.is_running() {
                signin.cancel();
            } else {
                self.signin = None;
            }
        } else if self.picker.is_some() {
            self.picker = None;
        } else if self.help.is_some() {
            self.help = None;
        } else if live.close_expand() {
            // The expand view owns input while open; Ctrl+C only closes it.
        } else if let Some(selection) = self.viewport.selected_text(transcript) {
            self.pending_copies.push(selection.into_boxed_str());
        } else {
            self.interrupt();
        }
    }

    /// Applies one key to the open sign-in overlay.
    fn apply_signin_key(&mut self, key: crate::keys::Key) {
        let Some(signin) = self.signin.as_mut() else {
            return;
        };
        match signin.key(key) {
            KeyAction::None => {}
            KeyAction::Close => self.signin = None,
            KeyAction::StartKey(typed) => {
                self.front.push(FrontRequest::StartKey {
                    provider: signin.provider().into(),
                    key: typed,
                });
            }
            KeyAction::RemoveAll => {
                self.signin = None;
                self.front.push(FrontRequest::LogoutAll);
            }
        }
    }

    fn accept_reply(&mut self, reply: Reply) -> Vec<String> {
        match reply {
            Reply::Accepted { turn, .. } => {
                self.active_turn = Some(turn);
                Vec::new()
            }
            Reply::Done(Output::Text(text) | Output::Markdown(text)) => {
                text.lines().map(crate::width::escape).collect()
            }
            Reply::Done(Output::Table(rows)) => rows
                .into_iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| crate::width::escape(cell))
                        .collect::<Vec<_>>()
                        .join("  ")
                })
                .collect(),
            Reply::Front(FrontAction::Quit) => {
                if self.composer.text().trim().is_empty() {
                    self.quit = true;
                } else {
                    // Focus starts on Keep: a blind Enter keeps the draft,
                    // and Esc lands on the same conservative verb.
                    self.dialog = Some(crate::dialog::DraftConfirm::default());
                }
                Vec::new()
            }
            Reply::Front(FrontAction::CopyReply { text }) => {
                self.pending_copies.push(text);
                Vec::new()
            }
            Reply::Front(FrontAction::ShowKeys) => crate::keys::help_labels()
                .into_iter()
                .map(|(key, label)| format!("{key} {label}"))
                .collect(),
            Reply::Front(FrontAction::Login { provider }) => {
                self.front.push(FrontRequest::Login(provider));
                Vec::new()
            }
            Reply::Front(FrontAction::Logout { provider }) => {
                self.front.push(FrontRequest::Logout(provider));
                Vec::new()
            }
            Reply::Front(action) => vec![crate::width::escape(&format!("{action:?}"))],
            Reply::Started(job) => vec![format!("Job {job} started.")],
            // A queued reply needs no row: the live block shows the queue.
            _ => Vec::new(),
        }
    }

    /// Takes the reply of the in-flight `Run` command once it has settled.
    fn settled_run(&mut self) -> Option<Result<Reply, TuiError>> {
        use std::sync::mpsc::TryRecvError;
        let settled = match self.inflight.as_ref()?.settled.try_recv() {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(TuiError::Terminal(
                "the command stopped without a reply".to_owned(),
            )),
        };
        self.inflight = None;
        Some(settled)
    }
}

/// A `Run` command executing on its own thread. Dropping it abandons the
/// wait and joins the worker, so no command thread outlives the loop that
/// owns the terminal and the host.
#[derive(Debug)]
struct InflightRun {
    settled: std::sync::mpsc::Receiver<Result<Reply, TuiError>>,
    abandon: Arc<tokio::sync::Notify>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for InflightRun {
    fn drop(&mut self) {
        self.abandon.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// One blob read call's worst-case wait; a wedged remote answers the expand
/// view with its timeout, never with silence.
const BLOB_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The worker reading a card's `TextBlob` parts off the loop thread.
/// Dropping it abandons the wait; the join is taken only when the worker
/// already finished, so a slow remote read never stalls the loop.
#[derive(Debug)]
struct BlobReadWork {
    abandon: Arc<tokio::sync::Notify>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for BlobReadWork {
    fn drop(&mut self) {
        self.abandon.notify_one();
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

/// Runs one `Run` command on its own thread. The thread blocks on the same
/// runtime handle the loop uses for every other host call, and gives up the
/// wait when the returned guard is dropped.
fn start_run<A: TuiAgent>(
    runtime: &tokio::runtime::Handle,
    agent: &A,
    command: Command,
) -> std::io::Result<InflightRun> {
    let (reply, settled) = std::sync::mpsc::channel();
    let abandon = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime.clone();
    let agent = agent.clone();
    let abandoned = Arc::clone(&abandon);
    let worker = std::thread::Builder::new()
        .name("dal-tui-command".to_owned())
        .spawn(move || {
            let outcome = runtime.block_on(async {
                let submit = std::pin::pin!(agent.submit(command));
                let given_up = std::pin::pin!(abandoned.notified());
                match futures::future::select(submit, given_up).await {
                    futures::future::Either::Left((reply, _)) => reply,
                    futures::future::Either::Right(((), _)) => Err(TuiError::Terminal(
                        "the command was abandoned when the terminal closed".to_owned(),
                    )),
                }
            });
            let _ = reply.send(outcome);
        })?;
    Ok(InflightRun {
        settled,
        abandon,
        worker: Some(worker),
    })
}

/// Reads each `TextBlob` the expanded card names on its own thread, one read
/// at a time and each under [`BLOB_READ_TIMEOUT`]. The sink is a `Weak`: when
/// the view closes, upgrades fail and the worker exits; `abandon` also cuts
/// an in-flight read on teardown. Every blob reads at most once — no per
/// frame refetch.
fn start_blob_load<A: TuiAgent>(
    runtime: &tokio::runtime::Handle,
    agent: &A,
    sink: &Weak<Mutex<HashMap<BlobId, BlobRead>>>,
) -> BlobReadWork {
    let abandon = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime.clone();
    let agent = agent.clone();
    let weak = sink.clone();
    let abandoned = Arc::clone(&abandon);
    let worker = std::thread::Builder::new()
        .name("dal-tui-blob-read".to_owned())
        .spawn(move || {
            let Some(sink) = weak.upgrade() else {
                return;
            };
            let ids: Vec<BlobId> = sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .copied()
                .collect();
            drop(sink);
            for id in ids {
                let read = runtime.block_on(async {
                    let read =
                        std::pin::pin!(tokio::time::timeout(BLOB_READ_TIMEOUT, agent.blob(id),));
                    let given_up = std::pin::pin!(abandoned.notified());
                    match futures::future::select(read, given_up).await {
                        futures::future::Either::Left((outcome, _)) => Some(outcome),
                        futures::future::Either::Right(((), _)) => None,
                    }
                });
                let Some(outcome) = read else {
                    return;
                };
                let write = match outcome {
                    Ok(Ok(bytes)) => {
                        BlobRead::Text(Arc::from(String::from_utf8_lossy(&bytes).as_ref()))
                    }
                    Ok(Err(error)) => BlobRead::Failed(Arc::from(error.to_string().as_str())),
                    Err(_) => BlobRead::Failed(Arc::from("the blob read timed out")),
                };
                let Some(sink) = weak.upgrade() else {
                    return;
                };
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(id, write);
            }
        });
    match worker {
        Ok(worker) => BlobReadWork {
            abandon,
            worker: Some(worker),
        },
        // No worker: resolve every pending slot so the view renders the
        // failure instead of reading forever.
        Err(error) => {
            if let Some(sink) = sink.upgrade() {
                let mut slots = sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let reason: Arc<str> =
                    Arc::from(format!("the blob reader failed to start: {error}").as_str());
                for slot in slots.values_mut() {
                    *slot = BlobRead::Failed(Arc::clone(&reason));
                }
            }
            BlobReadWork {
                abandon,
                worker: None,
            }
        }
    }
}

pub(super) fn run<H, M, S>(
    host: &H,
    opts: &TuiOptions,
    io: &impl TermIo,
    model_source: M,
    save_diagrams: S,
) -> Result<TuiExit, TuiError>
where
    H: TuiHost,
    M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    S: FnMut(bool) -> Result<(), TuiError>,
{
    crate::keys::validate(crate::keys::BINDINGS)?;
    if !opts.env.stdin_tty {
        return Err(crate::term::te_stdin_not_terminal());
    }
    if let crate::ThemeRequest::Named(name) = &opts.theme_request {
        crate::theme::resolve_name(name)?;
    }

    let agent = opts
        .rt
        .block_on(host.open(opts.session.clone(), ClientId::new("dal-tui")))?;
    let session_id = agent.session();
    let attach = || {
        let view = opts.rt.block_on(agent.view(snapshot_page()))?;
        let subscription = opts
            .rt
            .block_on(agent.subscribe(Some((view.r#gen, view.seq))))?;
        let commands = opts.rt.block_on(host.commands())?;
        let pump = Pump::spawn(&opts.rt, subscription);
        let state = Arc::new(Mutex::new(TermState::new()));
        let hook = PanicHookGuard::install(Arc::clone(&state));
        io.enable_raw()
            .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_raw(true);
        Ok::<_, TuiError>((view, commands, pump, state, hook))
    };
    let (view, commands, mut pump, state, hook) = match attach() {
        Ok(attached) => attached,
        Err(error) => {
            let _ = opts.rt.block_on(host.close(session_id));
            return Err(error);
        }
    };

    let outcome = run_loop(
        io,
        opts,
        &state,
        &agent,
        &mut pump,
        CommandContext {
            specs: commands,
            model_source,
            host: host.clone(),
        },
        save_diagrams,
        view,
    );
    restore(
        &state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    drop(hook);
    let running = outcome
        .as_ref()
        .ok()
        .and_then(|(_, turn, _)| *turn)
        .or_else(|| {
            opts.rt
                .block_on(agent.view(snapshot_page()))
                .ok()
                .and_then(|view| match view.turn {
                    TurnState::Running { turn } | TurnState::Settling { turn } => Some(turn),
                    _ => None,
                })
        });
    cancel_active(opts, &agent, io, &mut pump, running)?;
    drop(pump);
    let close = opts.rt.block_on(host.close(session_id));
    let (exit, _, rows) = outcome?;
    close?;
    write_exit(io, opts, session_id, &exit, rows)?;
    Ok(exit)
}

/// Cancels a turn that outlived the loop, then reports it on inline screens.
fn cancel_active<A: TuiAgent>(
    opts: &TuiOptions,
    agent: &A,
    io: &dyn TermIo,
    pump: &mut Pump<A::Subscription>,
    running: Option<TurnId>,
) -> Result<(), TuiError> {
    if let Some(turn) = running
        && opts
            .rt
            .block_on(agent.submit(Command::Cancel {
                scope: CancelScope::Turn(turn),
            }))
            .is_ok()
    {
        let cancelled = wait_for_cancelled(pump, turn, Duration::from_secs(3));
        if cancelled && !matches!(opts.screen, crate::Screen::Fullscreen) {
            io.write(
                format!(
                    "\x1b[{};1H\x1b[2K{}\x1b[0m\r\n",
                    1,
                    crate::copy::ids::TURN_CANCELLED
                )
                .as_bytes(),
            )
            .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
        }
    }
    Ok(())
}

/// Writes the transcript rows (fullscreen only) and the final exit line.
fn write_exit(
    io: &dyn TermIo,
    opts: &TuiOptions,
    session_id: dal_core::SessionId,
    exit: &TuiExit,
    rows: Vec<String>,
) -> Result<(), TuiError> {
    if opts.screen == crate::Screen::Fullscreen {
        for row in rows {
            io.write(format!("{row}\r\n").as_bytes())
                .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
        }
    }
    let line = if exit.ephemeral {
        crate::copy::render(crate::copy::ids::EXIT_EPHEMERAL, &[("bin", opts.binary)], 0)
    } else {
        // An unnamed session resumes by its id; a placeholder name resolves to nothing.
        let session = session_id.to_string();
        let name = exit.name.as_deref().unwrap_or(&session);
        let token = dal_core::resume_token(name, session_id);
        crate::copy::render(
            crate::copy::ids::EXIT_SAVED,
            &[
                ("bin", opts.binary),
                ("token", &token),
                ("name", name),
                ("n", &exit.messages.to_string()),
            ],
            exit.messages,
        )
    };
    io.write(format!("\r\n{line}\r\n").as_bytes())
        .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
    Ok(())
}

/// Builds the snapshot page request for the opening view.
fn snapshot_page() -> PageReq {
    PageReq::default()
}

/// The terminal states a panic must put right: one per live pass.
///
/// A process has one panic hook, and `take_hook` and `set_hook` panic when a
/// thread that is already panicking calls them. A pass that swapped the hook
/// in on attach and back on detach would therefore abort the process when a
/// panic unwound it (a panic inside a destructor during unwinding aborts), and
/// two passes alive together would overwrite each other's hook and could
/// leave a stale one behind. So the hook goes in once, for the process, and a
/// pass only lists its terminal state here while it owns the terminal.
struct PanicRestorers {
    live: Mutex<Vec<Arc<Mutex<TermState>>>>,
}

impl PanicRestorers {
    const fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
        }
    }

    fn add(&self, state: &Arc<Mutex<TermState>>) {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::clone(state));
    }

    /// Forgets `state`, so the list keeps no terminal its pass has released.
    fn remove(&self, state: &Arc<Mutex<TermState>>) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = live.iter().position(|held| Arc::ptr_eq(held, state)) {
            live.swap_remove(index);
        }
    }

    /// Hands `restore` the current modes of every live terminal. The list
    /// lock is released before any state is locked or written, so the hook
    /// never waits on a lock it can be holding while it runs.
    fn restore_all(&self, mut restore: impl FnMut(&TermState)) {
        let live: Vec<Arc<Mutex<TermState>>> = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for state in live {
            let saved = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            restore(&saved);
        }
    }
}

static PANIC_RESTORERS: PanicRestorers = PanicRestorers::new();
static PANIC_HOOK: std::sync::Once = std::sync::Once::new();

/// A pass's claim on the process panic hook: while it lives, a panic anywhere
/// in the process restores this pass's terminal before the previous hook
/// reports the panic.
///
/// The first guard puts the hook in; it stays for the life of the process and
/// always hands the panic on to the hook that was there before it. Dropping a
/// guard only forgets its terminal, which is safe while a panic unwinds.
struct PanicHookGuard {
    state: Arc<Mutex<TermState>>,
}

impl PanicHookGuard {
    fn install(state: Arc<Mutex<TermState>>) -> Self {
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                PANIC_RESTORERS.restore_all(restore);
                previous(info);
            }));
        });
        PANIC_RESTORERS.add(&state);
        Self { state }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        PANIC_RESTORERS.remove(&self.state);
    }
}

/// Runs the opening sequence: builds the surfaces, seeds the transcript,
/// resolves the theme, and lands the probed second paint.
fn boot<A: TuiAgent>(
    io: &dyn TermIo,
    opts: &TuiOptions,
    state: &Arc<Mutex<TermState>>,
    agent: &A,
    specs: std::sync::Arc<[dal_core::CommandSpec]>,
    view: dal_core::View,
) -> Result<
    (
        Surfaces,
        Painter,
        ResolvedTheme,
        crate::term::Probe,
        KeyDecoder,
    ),
    TuiError,
> {
    let mut decoder = KeyDecoder::default();
    let diagram_cache =
        crate::diagram::RenderCache::with_fonts(opts.env.path.clone(), opts.env.fonts.clone());
    let branch = crate::status::workspace_branch(
        agent.workspace_is_local(),
        view.session.workspace.as_path(),
    );
    let mut surfaces = Surfaces {
        session: Session {
            active_turn: match view.turn {
                TurnState::Running { turn } | TurnState::Settling { turn } => Some(turn),
                _ => None,
            },
            commands: specs,
            ..Session::default()
        },
        dialog: DialogUi::default(),
        live: Live::default(),
        transcript: Transcript::default(),
        diagram_settings: crate::diagram::DiagramSettings {
            enabled: opts.diagrams,
        },
        diagram_generation: diagram_cache.generation(),
        diagram_cache,
        columns: io.size().map_or(80, |(columns, _)| columns),
        branch,
        view,
    };
    surfaces.dialog.resync(surfaces.view.open.clone());
    surfaces.live.seed_ext_status(&agent.ext_status());
    surfaces.seed(opts);

    let debug_start = std::time::Instant::now();
    let mut painter = Painter::default();
    let palette = crate::theme::load(&crate::ThemeRequest::Palette, opts.color, None, None)?;
    if opts.env.debug {
        eprintln!("[t0] pre-paint {}ms", debug_start.elapsed().as_millis());
    }
    surfaces.paint(io, state, &mut painter, opts, &palette)?;
    let probe = probe_terminal(
        io,
        state,
        opts,
        &palette,
        &mut painter,
        &mut decoder,
        &mut surfaces,
    )?;
    if opts.env.debug {
        eprintln!("[t0] probe done {}ms", debug_start.elapsed().as_millis());
    }
    let theme = crate::theme::load(
        &opts.theme_request,
        opts.color,
        probe.background_luminance,
        opts.env.colorfgbg.as_deref(),
    )?;
    painter.set_image_rung(crate::image::resolve_rung(
        &opts.env,
        crate::image::ImageProbe {
            kitty_ok: probe.kitty_graphics,
            da1_sixel: probe.sixel,
        },
        opts.images || surfaces.diagram_settings.enabled,
    ));
    surfaces.paint(io, state, &mut painter, opts, &theme)?;
    if probe.kitty_keyboard {
        io.write(b"\x1b[>1u")
            .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_kitty(true);
    }
    if probe.grapheme_mode {
        io.write(b"\x1b[?2027h")
            .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_grapheme(true);
    }
    painter.set_sync(probe.sync_update);
    Ok((surfaces, painter, theme, probe, decoder))
}

fn run_loop<A, H, M, S>(
    io: &dyn TermIo,
    opts: &TuiOptions,
    state: &Arc<Mutex<TermState>>,
    agent: &A,
    pump: &mut Pump<A::Subscription>,
    commands: CommandContext<H, M>,
    mut save_diagrams: S,
    view: dal_core::View,
) -> Result<(TuiExit, Option<TurnId>, Vec<String>), TuiError>
where
    A: TuiAgent,
    H: TuiHost,
    M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    S: FnMut(bool) -> Result<(), TuiError>,
{
    let CommandContext {
        specs,
        mut model_source,
        host,
    } = commands;
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_paste(true);
    io.write(b"\x1b[?2004h")
        .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
    if opts.screen == crate::Screen::Fullscreen {
        io.write(b"\x1b[?1049h\x1b[?25l")
            .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_fullscreen(true);
    }
    io.write(&startup_probe(opts.images || opts.diagrams))
        .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;

    let (mut surfaces, mut painter, theme, probe, mut decoder) =
        boot(io, opts, state, agent, specs, view)?;
    let mut resolution_poll = Instant::now();
    loop {
        if io.shutdown_code().is_some() {
            break;
        }
        let bytes = io
            .read(Duration::from_millis(33))
            .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
        if io.take_resize()
            && let Ok((width, _)) = io.size()
        {
            surfaces.columns = width;
        }
        if io.shutdown_code().is_some() {
            break;
        }
        let now = Instant::now();
        let events = if bytes.is_empty() {
            decoder.tick(now)
        } else {
            decoder.feed(&bytes, now)
        };
        let (term_width, term_height) = io.size().unwrap_or((80, 24));
        for event in events {
            apply_event_at(
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &mut surfaces.live,
                &surfaces.view.entries.items,
                surfaces.diagram_settings,
                event,
                probe.kitty_keyboard,
                opts.screen,
                term_width,
                term_height,
            );
        }
        surfaces.flush_answers(opts, agent);
        surfaces.submit_commands(opts, agent, &mut model_source)?;
        surfaces.run_front(opts, &host, agent);
        // A closed expand view drops its sink; the read worker then has no
        // reason to exist and its guard is released here.
        if surfaces.live.expanded().is_none() {
            surfaces.session.blob_load = None;
        }
        surfaces.poll_signin(opts, io, &mut model_source);
        poll_model_retry(&mut surfaces.session, &mut model_source);
        if surfaces.session.suspend {
            surfaces.session.suspend = false;
            suspend_and_continue(io, state, opts, &theme, &mut painter, &mut surfaces)?;
        }
        surfaces.write_copies(io);
        surfaces.apply_diagram_updates(opts, &mut painter, &probe, &mut save_diagrams);
        surfaces.drain(opts, agent, pump)?;
        surfaces.poll_resolution(opts, agent, &mut resolution_poll);
        surfaces.reseed_diagrams(opts);
        // The arming clock runs only while the dialog can be painted: a
        // floored frame shows no dialog, so no key may arm off an unseen
        // question. Skipping the tick keeps a floored dialog in `Shown`
        // until resize re-arms it through the normal delay.
        if !crate::render::below_floor(term_width, term_height) {
            surfaces.dialog.tick(Instant::now());
        }
        if surfaces.request_quit() {
            break;
        }
        surfaces.live.spin(now);
        surfaces.paint(io, state, &mut painter, opts, &theme)?;
        if surfaces.session.cancel_settled
            || surfaces
                .session
                .cancel_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            break;
        }
    }

    // The caret rests hidden at the block top while the composer is empty;
    // the exit line continues from the caret, so move it below the block
    // first or the line overwrites a block row (a notice, the composer).
    if opts.screen == crate::Screen::Inline
        && let Err(error) = painter.rest_below_block(io)
    {
        return Err(error);
    }
    Ok(surfaces.finish(opts))
}

fn wait_for_cancelled<S: TuiSubscription>(
    pump: &mut Pump<S>,
    turn: TurnId,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match pump
            .deliveries
            .recv_timeout(remaining.min(Duration::from_millis(50)))
        {
            Ok(Ok(TuiDelivery::Update(update))) => {
                if let UpdateKind::TurnEnded { turn: ended, stop } = &update.kind
                    && *ended == turn
                {
                    return *stop == Stop::Cancelled;
                }
            }
            Ok(Ok(TuiDelivery::Resync(_)) | Err(_))
            | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    false
}

/// Deliveries the pump holds ahead of the 50 ms drain before it stops
/// reading the subscription.
const PUMP_DEPTH: usize = 1024;

/// How long the pump waits before retrying a send into a full queue.
const PUMP_BACKPRESSURE_WAIT: Duration = Duration::from_millis(5);

type PumpItem = Result<TuiDelivery, TuiError>;

/// Owned delivery pump: one parked thread turns the async subscription into
/// a sync channel the 50 ms input loop drains without blocking.
///
/// `run` is called outside async context; the pump thread inherits that same
/// freedom and drives the subscription with the process-edge runtime handle.
/// The channel holds at most [`PUMP_DEPTH`] deliveries. While the loop is busy
/// in a host call the pump waits with one delivery in hand and stops reading
/// the subscription, which keeps its own bounded queue and answers a lagging
/// reader with a resync, so no delivery is dropped here and none piles up
/// without limit. An error is queued like any delivery, so a full queue
/// never hides a failure. `Drop` ends the worker's wait immediately and
/// joins it, so neither shutdown nor resync leaks a thread.
struct Pump<S: TuiSubscription> {
    deliveries: std::sync::mpsc::Receiver<PumpItem>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_signal: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    marker: std::marker::PhantomData<S>,
}

/// Queues `item`, waiting while the queue is full. Returns false once the
/// consumer is gone or the pump was asked to stop.
fn deliver(
    sender: &std::sync::mpsc::SyncSender<PumpItem>,
    stop: &std::sync::atomic::AtomicBool,
    mut item: PumpItem,
) -> bool {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::TrySendError;
    loop {
        match sender.try_send(item) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(unsent)) => {
                if stop.load(Ordering::SeqCst) {
                    return false;
                }
                item = unsent;
                std::thread::sleep(PUMP_BACKPRESSURE_WAIT);
            }
        }
    }
}

impl<S: TuiSubscription> Pump<S> {
    fn spawn(runtime: &tokio::runtime::Handle, mut subscription: S) -> Self {
        use std::sync::atomic::Ordering;
        let (sender, deliveries) = std::sync::mpsc::sync_channel(PUMP_DEPTH);
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // A oneshot ends the wait the moment the pump drops. Polling the
        // subscription under a tokio timeout instead would register a timer
        // on a runtime that may already be shutting down and panic.
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("dal-tui-delivery-pump".to_owned())
            .spawn({
                let runtime = runtime.clone();
                let stop_flag = std::sync::Arc::clone(&stop);
                move || {
                    while !stop_flag.load(Ordering::SeqCst) {
                        let tick = runtime.block_on(async {
                            tokio::select! {
                                _ = &mut stop_rx => None,
                                item = subscription.next() => Some(item),
                            }
                        });
                        match tick {
                            // Stopped, or the stream closed/lagged and latches
                            // `None` — later calls return at once either way.
                            None | Some(Ok(None)) => break,
                            Some(Ok(Some(delivery))) => {
                                if !deliver(&sender, &stop_flag, Ok(delivery)) {
                                    break;
                                }
                            }
                            Some(Err(error)) => {
                                deliver(&sender, &stop_flag, Err(error));
                                break;
                            }
                        }
                    }
                }
            })
            .ok();
        // Without the worker the client still submits; live regions stay still.
        Self {
            deliveries,
            stop,
            stop_signal: std::sync::Mutex::new(Some(stop_tx)),
            worker,
            marker: std::marker::PhantomData,
        }
    }

    /// Stops the current worker and pumps a fresh cursor instead.
    fn restart(&mut self, runtime: &tokio::runtime::Handle, subscription: S) {
        *self = Self::spawn(runtime, subscription);
    }
}

impl<S: TuiSubscription> Drop for Pump<S> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(signal) = self.stop_signal.lock().ok().and_then(|mut s| s.take()) {
            let _ = signal.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Reads the kitty keyboard flag out of the shared terminal state.
fn terminal_kitty(state: &std::sync::Mutex<TermState>) -> bool {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .kitty()
}

/// The live surfaces the loop mutates — view, interaction state, and render
/// inputs move together through every drain.
struct Surfaces {
    view: dal_core::View,
    session: Session,
    dialog: DialogUi,
    live: Live,
    transcript: Transcript,
    columns: u16,
    diagram_settings: crate::diagram::DiagramSettings,
    diagram_cache: crate::diagram::RenderCache,
    diagram_generation: u64,
    branch: Option<String>,
}

impl Surfaces {
    /// Seeds the transcript, then re-seeds when the cache rolled mid-write.
    fn seed(&mut self, opts: &TuiOptions) {
        seed_transcript(
            &self.view,
            &mut self.transcript,
            self.columns,
            opts.env.width_mode,
            self.diagram_settings,
            &self.diagram_cache,
        );
        self.reseed_diagrams(opts);
    }

    /// Re-seeds the transcript when the diagram cache rolled to a new generation.
    fn reseed_diagrams(&mut self, opts: &TuiOptions) {
        let generation = self.diagram_cache.generation();
        if generation != self.diagram_generation {
            seed_transcript(
                &self.view,
                &mut self.transcript,
                self.columns,
                opts.env.width_mode,
                self.diagram_settings,
                &self.diagram_cache,
            );
            self.diagram_generation = generation;
        }
    }

    /// Paints one frame over the surfaces' current state.
    fn paint(
        &mut self,
        io: &dyn TermIo,
        state: &Arc<Mutex<TermState>>,
        painter: &mut Painter,
        opts: &TuiOptions,
        theme: &ResolvedTheme,
    ) -> Result<(), TuiError> {
        painter.paint(
            io,
            state,
            FrameInput {
                view: &self.view,
                screen: opts.screen,
                composer: self.session.composer.text(),
                cursor: self.session.composer.cursor(),
                exit_draft: self
                    .session
                    .dialog
                    .as_ref()
                    .map(crate::dialog::DraftConfirm::focused),
                branch: self.branch.as_deref(),
                popup: &self.session.popup,
                live: &self.live,
                dialog: &self.dialog,
                picker: self.session.picker.as_ref(),
                help: self.session.help.as_ref(),
                signin: self.session.signin.as_ref(),
                transcript: &self.transcript,
                viewport: &self.session.viewport,
                opts,
                kitty_keyboard: terminal_kitty(state),
                theme,
                diagram_settings: self.diagram_settings,
                diagram_cache: &self.diagram_cache,
            },
            self.session.overlay,
        )
    }

    /// Answers every request the input map resolved this pass.
    fn flush_answers<A: TuiAgent>(&mut self, opts: &TuiOptions, agent: &A) {
        for (id, answer) in std::mem::take(&mut self.session.pending_answers) {
            if let Err(error) = opts.rt.block_on(agent.answer(id, answer)) {
                // A losing answer or a dead host surfaces as an error with
                // the deck's notice prefix; `request.lost` text is the
                // host's own message, never a silent drop.
                self.live
                    .notice_as(crate::theme::Role::Error, format!("note: {error}"));
            }
        }
    }

    /// Folds one sequenced update into the live region, then the view,
    /// transcript, dialog, and session: the one path the loop's drain takes.
    fn absorb_update<A: TuiAgent>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        update: &dal_core::Update,
    ) -> Result<(), TuiError> {
        self.live.apply_update(update);

        self.view.r#gen = update.r#gen;
        self.view.seq = update.seq;
        self.apply_update(opts, agent, update)
    }

    /// Applies one update's kind to view, transcript, dialog, and session.
    fn apply_update<A: TuiAgent>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        update: &dal_core::Update,
    ) -> Result<(), TuiError> {
        match &update.kind {
            UpdateKind::Tree(delta) => {
                for entry in &delta.added {
                    if let dal_core::EntryKind::ToolResult { call, .. } = &entry.kind
                        && let Some(duration) = self.live.release_tool(call.as_str())
                    {
                        self.transcript.note_tool_duration(call.as_str(), duration);
                    }
                    commit_entry(
                        entry,
                        &mut self.transcript,
                        self.columns,
                        opts.env.width_mode,
                        self.diagram_settings,
                        &self.diagram_cache,
                    );
                    if !self
                        .view
                        .entries
                        .items
                        .iter()
                        .any(|prior| prior.id == entry.id)
                    {
                        self.view.entries.items.push(entry.clone());
                    }
                }
            }
            UpdateKind::RequestOpened(request) => {
                self.dialog.opened(request.clone());
                self.view.open.push(request.clone());
            }
            UpdateKind::RequestResolved {
                id,
                answer,
                by,
                cause,
            } => {
                // Only a dialog this client saw (open now, or answered here
                // before a snapshot dropped it) announces the outcome; queued
                // requests the user never saw stay silent.
                if let Some(title) = self.dialog.resolved(&id.to_string()) {
                    self.live
                        .notice(resolution_notice(*cause, by, answer, &title));
                }
                self.view.open.retain(|request| request.id != *id);
            }
            UpdateKind::TurnStarted { turn, .. } => {
                self.view.turn = TurnState::Running { turn: *turn };
                self.session.active_turn = Some(*turn);
            }
            UpdateKind::TurnEnded { stop, .. } => {
                // A turn may have switched branches; the file read is one short line.
                self.branch = crate::status::workspace_branch(
                    agent.workspace_is_local(),
                    self.view.session.workspace.as_path(),
                );
                self.session.active_turn = None;
                if self.session.cancelling.is_some() && *stop == Stop::Cancelled {
                    self.session.cancel_settled = true;
                }
                self.live.take_assistant_text();
                let fresh = opts.rt.block_on(agent.view(snapshot_page()))?;
                seed_transcript(
                    &fresh,
                    &mut self.transcript,
                    self.columns,
                    opts.env.width_mode,
                    self.diagram_settings,
                    &self.diagram_cache,
                );
                self.dialog.resync(fresh.open.clone());
                self.view = fresh;
            }
            UpdateKind::Settings(settings) => {
                self.view.session.name.clone_from(&settings.name);
                self.view.settings = settings.clone();
            }
            UpdateKind::Usage(usage) => self.view.usage = usage.clone(),
            _ => {}
        }
        Ok(())
    }

    /// Applies every queued delivery; resubscribes with a fresh pump on resync.
    fn drain<A: TuiAgent>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        pump: &mut Pump<A::Subscription>,
    ) -> Result<(), TuiError> {
        while let Ok(delivery) = pump.deliveries.try_recv() {
            match delivery? {
                TuiDelivery::Update(update) => self.absorb_update(opts, agent, &update)?,
                TuiDelivery::Resync(snapshot) => {
                    let remote_repaint = snapshot.is_some();
                    let fresh = match snapshot {
                        Some(snapshot_view) => *snapshot_view,
                        None => opts.rt.block_on(agent.view(snapshot_page()))?,
                    };
                    if !remote_repaint {
                        let subscription = opts
                            .rt
                            .block_on(agent.subscribe(Some((fresh.r#gen, fresh.seq))))?;
                        pump.restart(&opts.rt, subscription);
                    }
                    self.live.reset_after_resync();
                    self.live.seed_ext_status(&agent.ext_status());
                    seed_transcript(
                        &fresh,
                        &mut self.transcript,
                        self.columns,
                        opts.env.width_mode,
                        self.diagram_settings,
                        &self.diagram_cache,
                    );
                    self.dialog.resync(fresh.open.clone());
                    self.session.active_turn = match fresh.turn {
                        TurnState::Running { turn } | TurnState::Settling { turn } => Some(turn),
                        _ => None,
                    };
                    self.view = fresh;
                    break;
                }
            }
        }
        Ok(())
    }

    /// Submits the commands the input map queued, folding each host reply
    /// into a picker, transcript rows, or the session reply path.
    ///
    /// A `Run` command goes to its own thread, one at a time: a plugin
    /// handler may raise a question (a grant, an ask) that only this loop can
    /// show and answer, so waiting inline for its reply would keep the
    /// question off screen until it timed out. Later commands wait behind it;
    /// `Cancel` never waits.
    fn submit_commands<A, M>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        model_source: &mut M,
    ) -> Result<(), TuiError>
    where
        A: TuiAgent,
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        if let Some(settled) = self.session.settled_run() {
            self.fold_submit_reply(opts, agent, model_source, settled)?;
        }
        for command in std::mem::take(&mut self.session.pending_commands) {
            if let Command::Cancel { .. } = command {
                self.session.cancelling = None;
            } else if self.session.inflight.is_some() {
                self.session.pending_commands.push(command);
                continue;
            }
            if let Command::Run { .. } = command {
                match start_run(&opts.rt, agent, command) {
                    Ok(settled) => self.session.inflight = Some(settled),
                    Err(error) => self.live.notice_as(
                        crate::theme::Role::Error,
                        format!("note: cannot start the command: {error}"),
                    ),
                }
                continue;
            }
            let submit_reply = opts.rt.block_on(agent.submit(command));
            self.fold_submit_reply(opts, agent, model_source, submit_reply)?;
        }
        Ok(())
    }

    /// Folds one host reply into a picker, transcript rows, or the session
    /// reply path.
    fn fold_submit_reply<A, M>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        model_source: &mut M,
        submit_reply: Result<Reply, TuiError>,
    ) -> Result<(), TuiError>
    where
        A: TuiAgent,
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        let submit_reply = match submit_reply {
            Ok(submit_reply) => submit_reply,
            Err(error) => {
                // Command and prompt failures keep the notice prefix and the
                // error role so the row reads as a failure, never dim text.
                self.live
                    .notice_as(crate::theme::Role::Error, format!("note: {error}"));
                return Ok(());
            }
        };
        let queued = matches!(&submit_reply, Reply::Queued { .. });
        let reply_rows = match submit_reply {
            Reply::Choose { chooser, filter } => {
                self.session.picker = None;
                let picker = match chooser {
                    Chooser::Tree => Some(crate::picker::tree_picker(&self.view, &filter)),
                    Chooser::ForkPoint => {
                        let entries = fork_entries(opts, agent)?;
                        Some(crate::picker::fork_picker(&entries, &filter))
                    }
                    Chooser::Model => {
                        if let Ok(models) = model_source() {
                            Some(crate::picker::model_picker(&models, &filter))
                        } else {
                            Some(crate::picker::model_picker_failed(&filter))
                        }
                    }
                    Chooser::Settings => Some(crate::picker::settings_picker(
                        self.diagram_settings.enabled,
                        &filter,
                    )),
                    Chooser::Login => Some(signin::login_picker(&filter)),
                    Chooser::Logout => {
                        self.session.front.push(FrontRequest::LogoutPicker(filter));
                        None
                    }
                    Chooser::ScopedModels => {
                        self.scoped_models_choice(opts, agent, &filter, model_source)
                    }
                    _ => {
                        self.live
                            .notice(crate::copy::ids::PICKER_UNSUPPORTED.to_owned());
                        None
                    }
                };
                self.session.picker = picker;
                Vec::new()
            }
            // `/new` needs a session this terminal cannot open in place; the
            // refusal is a failure, so it carries the error role and `note:`
            // prefix like every other command failure.
            Reply::Front(FrontAction::NewSession) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    "note: /new opens a fresh session outside this terminal; use /quit then reopen",
                );
                Vec::new()
            }
            other => self.session.accept_reply(other),
        };
        if queued {
            self.view = opts.rt.block_on(agent.view(snapshot_page()))?;
        }
        self.print_rows(&reply_rows);
        Ok(())
    }

    /// Commits output rows to the transcript under the next command id.
    fn print_rows(&mut self, rows: &[String]) {
        if rows.is_empty() {
            return;
        }
        let rows: Vec<String> = rows.iter().map(|row| format!("  {row}")).collect();
        self.session.command_seq = self.session.command_seq.saturating_add(1);
        self.transcript
            .commit(&format!("command-{}", self.session.command_seq), &rows);
    }

    /// Builds the scoped-models picker from live catalog rows and the
    /// committed scope the agent reports right now. The terminal keeps no
    /// scope of its own: every open reads `settings.scoped_models` from a
    /// fresh `Agent::view`, where `None` (never committed) and `Some([])`
    /// (explicitly cleared) both mean no scope. Only scope-eligible rows
    /// (`scoped_id: Some`) become rows; synthetic, harness, and legacy rows
    /// are excluded so the gate can match every offered id. Active scope
    /// strings compare verbatim; an active id the list does not name shows as
    /// its own checked row, so it stays until unchecked and can be removed.
    /// A failing model source or view read shows
    /// the failure and opens no picker: an unknown scope is never guessed
    /// as empty, because submitting it would clear the real restriction.
    fn scoped_models_choice<A, M>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        filter: &str,
        model_source: &mut M,
    ) -> Option<crate::picker::PickerUi>
    where
        A: TuiAgent,
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        let models = match model_source() {
            Ok(models) => models,
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!("note: cannot list models for the scope picker: {error}"),
                );
                return None;
            }
        };
        let fresh = match opts.rt.block_on(agent.view(snapshot_page())) {
            Ok(fresh) => fresh,
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!("note: cannot read the current model scope: {error}"),
                );
                return None;
            }
        };
        let rows = models
            .into_iter()
            .filter_map(|model| {
                model.scoped_id.map(|id| crate::picker::ScopedModelRow {
                    id,
                    label: model.label,
                })
            })
            .collect::<Vec<_>>();
        let active: &[Box<str>] = fresh.settings.scoped_models.as_deref().unwrap_or(&[]);
        Some(crate::picker::scoped_models_picker(rows, active, filter))
    }

    /// Runs the host- and agent-backed requests the input pass queued.
    fn run_front<H: TuiHost, A: TuiAgent>(&mut self, opts: &TuiOptions, host: &H, agent: &A) {
        for request in std::mem::take(&mut self.session.front) {
            match request {
                FrontRequest::Login(provider) => self.begin_login(opts, host, &provider),
                FrontRequest::StartKey { provider, key } => {
                    self.session.signin = Some(SignIn::flow(
                        host,
                        &opts.rt,
                        &provider,
                        Method::ApiKey,
                        Some(key),
                    ));
                }
                FrontRequest::LogoutPicker(filter) => self.logout_picker(opts, host, &filter),
                FrontRequest::Logout(provider) => self.logout(opts, host, Some(&provider)),
                FrontRequest::LogoutAll => self.logout(opts, host, None),
                FrontRequest::LoadBlobs(sink) => {
                    self.session.blob_load = Some(start_blob_load(&opts.rt, agent, &sink));
                }
            }
        }
    }

    /// Opens the sign-in overlay for `provider`: the key prompt, or the flow.
    fn begin_login<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H, provider: &str) {
        self.session.picker = None;
        self.session.signin = match signin::preferred_method(provider) {
            Some(Method::ApiKey) => Some(SignIn::key_prompt(provider)),
            Some(method) => Some(SignIn::flow(host, &opts.rt, provider, method, None)),
            None => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!(
                        "note: unknown provider \"{}\".",
                        crate::width::escape(provider)
                    ),
                );
                None
            }
        };
    }

    /// Opens the logout picker, or reports an empty store.
    fn logout_picker<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H, filter: &str) {
        match opts.rt.block_on(host.stored_credentials()) {
            Ok(stored) if stored.is_empty() => {
                self.print_rows(&[crate::copy::ids::LOGOUT_NONE.to_owned()]);
            }
            Ok(stored) => self.session.picker = Some(signin::logout_picker(&stored, filter)),
            Err(error) => self.print_rows(&signin::failure_rows(&error)),
        }
    }

    /// Removes one provider's credential, or every credential.
    fn logout<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H, provider: Option<&str>) {
        match opts.rt.block_on(host.logout(provider)) {
            Ok(removed) => {
                let saved = self
                    .view
                    .settings
                    .model
                    .as_ref()
                    .map(crate::picker::route_label)
                    .or_else(|| opts.default_model.as_deref().map(str::to_owned));
                self.print_rows(&signin::logout_rows(provider, &removed, saved.as_deref()));
            }
            Err(error) => self.print_rows(&signin::failure_rows(&error)),
        }
    }

    /// Drains the running sign-in and shows its one outcome.
    fn poll_signin<M>(&mut self, opts: &TuiOptions, io: &dyn TermIo, model_source: &mut M)
    where
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        let Some(finished) = self.session.signin.as_mut().and_then(|flow| flow.poll(io)) else {
            return;
        };
        self.session.signin = None;
        match finished.outcome {
            Ok(_) => self.signed_in(opts, &finished.provider, model_source),
            Err(error) => self.print_rows(&signin::failure_rows(&error)),
        }
    }

    /// Refreshes the model list after a sign-in. With no saved model the
    /// list opens as the model picker; otherwise the output is the
    /// signed-in line.
    fn signed_in<M>(&mut self, opts: &TuiOptions, provider: &str, model_source: &mut M)
    where
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        let needs_model = self.view.settings.model.is_none() && opts.default_model.is_none();
        let line = crate::copy::render(
            crate::copy::ids::LOGIN_SIGNED_IN,
            &[("provider", &crate::width::escape(provider))],
            1,
        );
        match model_source() {
            Ok(models) if needs_model => {
                self.session.picker = Some(crate::picker::model_picker(&models, ""));
            }
            Ok(_) => self.print_rows(&[line]),
            Err(_) => {
                self.session.picker = Some(crate::picker::model_picker_failed(""));
                self.print_rows(&[line]);
            }
        }
    }

    /// Sends each requested copy to the terminal clipboard (OSC 52) and
    /// reports the outcome in a notice.
    fn write_copies(&mut self, io: &dyn TermIo) {
        use base64::Engine as _;

        for text in std::mem::take(&mut self.session.pending_copies) {
            let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
            match io.write(format!("\x1b]52;c;{payload}\x07").as_bytes()) {
                Ok(()) => {
                    let count = text.chars().count();
                    let notice = crate::copy::render(
                        crate::copy::ids::COPY_DONE,
                        &[("n", &count.to_string())],
                        u64::try_from(count).unwrap_or(u64::MAX),
                    );
                    self.live.notice(notice);
                }
                Err(_) => {
                    // A refused clipboard write is a failure the user asked
                    // for, so it carries the error role, not dim text.
                    self.live
                        .notice_as(crate::theme::Role::Error, crate::copy::ids::COPY_FAILED);
                }
            }
        }
    }

    /// Applies queued diagram toggles: rung re-resolution and the optional
    /// persisted save.
    fn apply_diagram_updates<S: FnMut(bool) -> Result<(), TuiError>>(
        &mut self,
        opts: &TuiOptions,
        painter: &mut Painter,
        probe: &crate::term::Probe,
        save_diagrams: &mut S,
    ) {
        for (enabled, save) in std::mem::take(&mut self.session.pending_diagram_settings) {
            self.diagram_settings.enabled = enabled;
            painter.set_image_rung(crate::image::resolve_rung(
                &opts.env,
                crate::image::ImageProbe {
                    kitty_ok: probe.kitty_graphics,
                    da1_sixel: probe.sixel,
                },
                opts.images || enabled,
            ));
            if save == dal_core::command::Save::SessionAndDefault {
                match save_diagrams(enabled) {
                    Ok(()) => self.live.notice("diagram setting saved to dal.toml"),
                    Err(error) => self
                        .live
                        .notice_as(crate::theme::Role::Error, format!("note: {error}")),
                }
            } else {
                self.live.notice(if enabled {
                    "diagram rendering enabled for this session"
                } else {
                    "diagram rendering disabled for this session"
                });
            }
        }
    }

    /// Polls the open-request page while the dialog awaits resolution.
    ///
    /// The poll is the fallback for a lost `RequestResolved` update, so it
    /// starts one full interval after the answer was sent: the pump gets the
    /// first chance to deliver the resolution before a snapshot can drop the
    /// answered dialog.
    fn poll_resolution<A: TuiAgent>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        resolution_poll: &mut Instant,
    ) {
        let now = Instant::now();
        if !self.dialog.awaiting_resolution() {
            *resolution_poll = now + RESOLUTION_POLL_INTERVAL;
            return;
        }
        if now < *resolution_poll {
            return;
        }
        *resolution_poll = now + RESOLUTION_POLL_INTERVAL;
        let view_request = snapshot_page();
        let refreshed = opts.rt.block_on(async {
            tokio::time::timeout(RESOLUTION_POLL_INTERVAL, agent.view(view_request)).await
        });
        if let Ok(Ok(fresh)) = refreshed {
            self.dialog.resync(fresh.open);
            self.session.active_turn = match fresh.turn {
                TurnState::Running { turn } | TurnState::Settling { turn } => Some(turn),
                _ => None,
            };
        }
    }

    /// Turns a quit request into a turn cancellation when a turn is live;
    /// returns true when the loop should exit.
    fn request_quit(&mut self) -> bool {
        if !self.session.quit {
            return false;
        }
        if let Some(turn) = self.session.active_turn {
            self.session.pending_commands.push(Command::Cancel {
                scope: CancelScope::Turn(turn),
            });
            self.session.quit = false;
            self.session.cancelling = Some(turn);
            self.session.cancel_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
            return false;
        }
        true
    }

    /// Folds the loop state into the exit triple the caller restores from.
    fn finish(self, opts: &TuiOptions) -> (TuiExit, Option<TurnId>, Vec<String>) {
        let count = u64::try_from(self.view.entries.items.len()).unwrap_or(u64::MAX);
        let rows = self.transcript.rows().to_vec();
        let exit = TuiExit {
            session: Some(self.view.session.id),
            name: self.view.session.name,
            messages: count,
            ephemeral: matches!(opts.session, SessionRef::Ephemeral { .. }),
        };
        (exit, self.session.active_turn, rows)
    }
}

fn seed_transcript(
    view: &dal_core::View,
    transcript: &mut Transcript,
    columns: u16,
    mode: crate::WidthMode,
    diagram_settings: crate::diagram::DiagramSettings,
    diagram_cache: &crate::diagram::RenderCache,
) {
    for entry in &view.entries.items {
        if transcript.is_committed(&entry.id.to_string()) {
            continue;
        }
        commit_entry(
            entry,
            transcript,
            columns,
            mode,
            diagram_settings,
            diagram_cache,
        );
        if transcript.pending_rows().any(|row| row.pending_diagram) {
            break;
        }
    }
}

fn commit_entry(
    entry: &dal_core::EntryView,
    transcript: &mut Transcript,
    columns: u16,
    mode: crate::WidthMode,
    diagram_settings: crate::diagram::DiagramSettings,
    diagram_cache: &crate::diagram::RenderCache,
) {
    let entry_id = entry.id.to_string();
    // A committed entry is frozen; a pending one holds the transcript open
    // so later entries keep their journal order until the renderer lands.
    if transcript.is_committed(&entry_id) {
        transcript.clear_pending(&entry_id);
        return;
    }
    let duration = match &entry.kind {
        dal_core::EntryKind::ToolResult {
            call, elapsed_ms, ..
        } => transcript
            .tool_duration(call.as_str())
            .or_else(|| elapsed_ms.map(Duration::from_millis)),
        _ => None,
    };
    let rows = entry_rows_timed(
        entry,
        columns,
        mode,
        diagram_settings,
        diagram_cache,
        duration,
    );
    let pending = rows.iter().any(|row| row.pending_diagram);
    if pending {
        transcript.set_pending(
            &entry_id,
            rows.into_iter().filter(|row| row.pending_diagram).collect(),
        );
        return;
    }
    transcript.clear_pending(&entry_id);
    transcript.commit_rendered(&entry_id, &rows);
}

fn fork_entries<A: TuiAgent>(
    opts: &TuiOptions,
    agent: &A,
) -> Result<Vec<dal_core::EntryView>, TuiError> {
    let limit = NonZeroU32::new(PageReq::MAX_LIMIT).unwrap_or(NonZeroU32::MAX);
    let mut pages = Vec::new();
    let mut before = None;
    loop {
        let request =
            PageReq::new(limit, before).map_err(|error| TuiError::Terminal(error.to_string()))?;
        let page = opts.rt.block_on(agent.view(request))?;
        let next = page.entries.next_before;
        if next.is_some() && next == before {
            return Err(TuiError::Terminal(
                "dal-tui: session history did not advance".to_owned(),
            ));
        }
        pages.push(page.entries.items);
        before = next;
        if before.is_none() {
            break;
        }
    }
    let count = pages.iter().map(Vec::len).sum();
    let mut entries = Vec::with_capacity(count);
    for page in pages.into_iter().rev() {
        entries.extend(page);
    }
    Ok(entries)
}

/// Reads the startup probe window, streaming type-ahead into the decoder as
/// each read lands and painting an immediate frame for the keys it applies,
/// so bytes typed during the probe show at once instead of after the
/// deadline. Probe replies never reach the decoder, and the kitty switch
/// happens only after the probe, so window bytes decode under the
/// terminal's current encoding.
fn probe_terminal(
    io: &dyn TermIo,
    state: &Arc<Mutex<TermState>>,
    opts: &TuiOptions,
    palette: &ResolvedTheme,
    painter: &mut Painter,
    decoder: &mut KeyDecoder,
    surfaces: &mut Surfaces,
) -> Result<crate::term::Probe, TuiError> {
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut parser = ReplyParser::default();
    let mut probe = crate::term::Probe::default();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let (probe_width, probe_height) = io.size().unwrap_or((80, 24));
            apply_type_ahead_at(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &mut surfaces.live,
                &surfaces.view.entries.items,
                surfaces.diagram_settings,
                &parser.finish(),
                opts.screen,
                probe_width,
                probe_height,
            );
            break;
        }
        let bytes = io
            .read(remaining.min(Duration::from_millis(50)))
            .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
        if bytes.is_empty() {
            continue;
        }
        let (next, keys) = parser.feed(&bytes);
        probe.sync_update |= next.sync_update;
        probe.grapheme_mode |= next.grapheme_mode;
        probe.kitty_keyboard |= next.kitty_keyboard;
        probe.kitty_graphics |= next.kitty_graphics;
        probe.sixel |= next.sixel;
        probe.da1 |= next.da1;
        if next.background_luminance.is_some() {
            probe.background_luminance = next.background_luminance;
        }
        if !keys.is_empty() {
            let (probe_width, probe_height) = io.size().unwrap_or((80, 24));
            apply_type_ahead_at(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &mut surfaces.live,
                &surfaces.view.entries.items,
                surfaces.diagram_settings,
                &keys,
                opts.screen,
                probe_width,
                probe_height,
            );
            // A key event requests an immediate frame that bypasses the batch.
            surfaces.paint(io, state, painter, opts, palette)?;
        }
        if probe.da1 {
            let held = parser.finish();
            let (probe_width, probe_height) = io.size().unwrap_or((80, 24));
            apply_type_ahead_at(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &mut surfaces.live,
                &surfaces.view.entries.items,
                surfaces.diagram_settings,
                &held,
                opts.screen,
                probe_width,
                probe_height,
            );
            break;
        }
    }
    Ok(probe)
}

/// Decodes terminal bytes into key events and applies them at the current
/// terminal size, so startup type-ahead below the usable floor fails closed
/// exactly like main-loop input: only Ctrl+C is admitted.
fn apply_type_ahead_at(
    decoder: &mut KeyDecoder,
    session: &mut Session,
    dialog: &mut DialogUi,
    transcript: &Transcript,
    live: &mut Live,
    entries: &[dal_core::EntryView],
    diagrams: crate::diagram::DiagramSettings,
    bytes: &[u8],
    screen: crate::Screen,
    width: u16,
    height: u16,
) {
    if bytes.is_empty() {
        return;
    }
    for event in decoder.feed(bytes, Instant::now()) {
        apply_event_at(
            session, dialog, transcript, live, entries, diagrams, event, false, screen, width,
            height,
        );
    }
}

/// Paste bytes land in the surface that owns the input; an overlay with no
/// text field consumes them so nothing reaches the composer behind it.
fn apply_paste(session: &mut Session, dialog: &mut DialogUi, live: &Live, bytes: &[u8]) {
    if dialog.is_open() {
        dialog.paste(bytes);
        return;
    }
    if let Some(signin) = session.signin.as_mut() {
        signin.paste(&String::from_utf8_lossy(bytes));
        return;
    }
    if let Some(picker) = session.picker.as_mut() {
        // The picker's one-line filter is a text field: paste filters like
        // typed characters and never submits.
        picker.paste(&String::from_utf8_lossy(bytes));
        return;
    }
    let composer_owns_input = !session.overlay
        && live.expanded().is_none()
        && session.dialog.is_none()
        && session.picker.is_none()
        && session.help.is_none();
    if composer_owns_input {
        session.composer.insert_paste(bytes);
        session.update_popup();
    }
}

/// Help keys: the key table owns the input. Esc leaves; PageUp/PageDown
/// scroll one page and the arrows one row, so every binding stays reachable
/// on short terminals; everything else changes nothing, so no typed text
/// reaches the composer behind the overlay and no answer is ever queued.
/// Ctrl+C already closed the help in `interrupt_or_dismiss` before this.
fn apply_help_key(session: &mut Session, key: crate::keys::Key, kitty: bool) {
    use crossterm::event::KeyCode;

    if key.code == KeyCode::Esc {
        session.help = None;
        return;
    }
    let Some(help) = session.help.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Up => help.line_up(),
        KeyCode::Down => help.line_down(),
        _ => match resolve_in(key, kitty, &[Owner::Transcript]) {
            Some(Action::TranscriptPageUp) => help.page_up(),
            Some(Action::TranscriptPageDown) => help.page_down(),
            _ => {}
        },
    }
}

/// Overlay keys: the read-only overlay owns the input. Esc or q leaves and
/// the live block repaints in place; Ctrl+T toggles back; the transcript
/// keys scroll the shared viewport; everything else changes nothing, so no
/// typed text reaches the composer behind the overlay.
fn apply_overlay_key(
    session: &mut Session,
    transcript: &Transcript,
    key: crate::keys::Key,
    kitty: bool,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let rows = transcript.rows().len();
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _)
        | (KeyCode::Char('q'), KeyModifiers::NONE)
        | (KeyCode::Char('t'), KeyModifiers::CONTROL) => session.overlay = false,
        _ => match resolve_in(key, kitty, &[Owner::Transcript]) {
            Some(Action::TranscriptPageUp) => session.viewport.scroll_up(rows),
            Some(Action::TranscriptPageDown) => session.viewport.scroll_down(rows),
            Some(Action::TranscriptJumpLatest) => session.viewport.jump_latest(),
            _ => {}
        },
    }
}

/// Filter-row keys: Enter jumps to the latest match, Esc closes and returns
/// focus to the composer, and characters edit the query.
fn apply_search_key(session: &mut Session, transcript: &Transcript, key: crate::keys::Key) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Esc => session.viewport.close_search(),
        KeyCode::Enter => {
            let query = session
                .viewport
                .search_text()
                .unwrap_or_default()
                .to_owned();
            jump_to_search_match(session, transcript, &query);
        }
        KeyCode::Backspace => session.viewport.backspace_search(),
        KeyCode::Char(character)
            if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
        {
            session.viewport.type_search(character);
        }
        _ => {}
    }
}

/// Jumps the viewport to the latest transcript row containing the query.
fn jump_to_search_match(session: &mut Session, transcript: &Transcript, query: &str) {
    if query.is_empty() {
        return;
    }
    let needle = query.to_lowercase();
    if let Some(row) = transcript
        .rows()
        .iter()
        .rposition(|row| row.to_lowercase().contains(&needle))
    {
        session.viewport.jump_to(row, transcript.rows().len());
    }
}

/// Input contexts that resolve a key, in owner order; a viewport adds the
/// transcript keys.
fn key_owners(viewport: bool) -> &'static [Owner] {
    if viewport {
        &[
            Owner::App,
            Owner::Transcript,
            Owner::Composer,
            Owner::Editor,
        ]
    } else {
        &[Owner::App, Owner::Composer, Owner::Editor]
    }
}

/// Applies the caret and kill-ring edits; returns whether the action was one.
fn apply_edit_action(composer: &mut Composer, action: Action) -> bool {
    match action {
        Action::CharLeft => composer.move_left(),
        Action::CharRight => composer.move_right(),
        Action::WordLeft => composer.word_left(),
        Action::WordRight => composer.word_right(),
        Action::LineStart => composer.line_start(),
        Action::LineEnd => composer.line_end(),
        Action::DeleteWordBack => composer.delete_word_back(),
        Action::DeleteWordForward => composer.delete_word_forward(),
        Action::KillLineStart => composer.kill_line_start(),
        Action::KillLineEnd => composer.kill_line_end(),
        Action::Yank => composer.yank(),
        Action::Undo => composer.undo(),
        _ => return false,
    }
    true
}

/// Applies the actions a transcript viewport owns; returns whether it did.
fn apply_viewport_action(
    session: &mut Session,
    transcript: &Transcript,
    action: Action,
    screen: crate::Screen,
) -> bool {
    let rows = transcript.rows().len();
    match action {
        // The overlay is an inline-mode surface; fullscreen is already one.
        Action::TranscriptOverlay if screen == crate::Screen::Inline => {
            // Each opening starts at the live edge.
            session.viewport = crate::screen::fullscreen::Viewport::following();
            session.overlay = true;
        }
        Action::SearchTranscript if screen == crate::Screen::Fullscreen => {
            session.viewport.open_search();
        }
        Action::TranscriptPageUp => session.viewport.scroll_up(rows),
        Action::TranscriptPageDown => session.viewport.scroll_down(rows),
        Action::TranscriptJumpLatest => session.viewport.jump_latest(),
        Action::SelectionLeft => {
            session
                .viewport
                .extend_selection(SelectionStep::Left, transcript);
        }
        Action::SelectionRight => {
            session
                .viewport
                .extend_selection(SelectionStep::Right, transcript);
        }
        Action::SelectionUp => {
            session
                .viewport
                .extend_selection(SelectionStep::Up, transcript);
        }
        Action::SelectionDown => {
            session
                .viewport
                .extend_selection(SelectionStep::Down, transcript);
        }
        _ => return false,
    }
    true
}

/// Applies one input event at the current terminal size. While the surface
/// is below the usable floor the frame shows only the narrow warning plus
/// status, so admission fails closed: every key but Ctrl+C (and every paste)
/// is dropped before any dialog, picker, sign-in, help, overlay, expand,
/// search, or composer state can change. Ctrl+C still reaches
/// `interrupt_or_dismiss` so a floored loop stays interruptible; resize back
/// to a usable size re-arms every surface with its state intact.
fn apply_event_at(
    session: &mut Session,
    dialog: &mut DialogUi,
    transcript: &Transcript,
    live: &mut Live,
    entries: &[dal_core::EntryView],
    diagrams: crate::diagram::DiagramSettings,
    event: InputEvent,
    kitty: bool,
    screen: crate::Screen,
    width: u16,
    height: u16,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    if crate::render::below_floor(width, height) {
        let interrupt = matches!(
            &event,
            InputEvent::Key(key)
                if key.code == KeyCode::Char('c')
                    && key.modifiers == KeyModifiers::CONTROL
        );
        if !interrupt {
            return;
        }
    }

    let InputEvent::Key(key) = event else {
        if let InputEvent::Paste(bytes) = event {
            session.viewport.clear_selection();
            apply_paste(session, dialog, live, &bytes);
        }
        return;
    };
    // A modal surface arriving over the expand view closes it first so the
    // overlay rows never have two owners — and before Ctrl+C below, so the
    // view the user cannot see never swallows the interrupt its row order
    // already gave to the modal.
    if live.expanded().is_some()
        && (dialog.is_open()
            || session.dialog.is_some()
            || session.signin.is_some()
            || session.picker.is_some()
            || session.help.is_some()
            || session.overlay)
    {
        live.close_expand();
    }
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        session.interrupt_or_dismiss(live, transcript);
        return;
    }
    if let Some(mut confirm) = session.dialog.take() {
        match confirm.key(key) {
            Some(crate::dialog::DraftChoice::Discard) => session.discard_draft(),
            // `Keep` answers the question: the dialog closes and the draft stays.
            Some(crate::dialog::DraftChoice::Keep) => {}
            // A focus move or an ignored key leaves the question open.
            None => session.dialog = Some(confirm),
        }
        return;
    }
    if dialog.is_open() {
        if let Some(answer) = dialog.key(key) {
            session.pending_answers.push(answer);
        }
        return;
    }
    if session.signin.is_some() {
        session.apply_signin_key(key);
        return;
    }
    if session.picker.is_some() {
        apply_picker_key(session, key);
        return;
    }
    if session.help.is_some() {
        apply_help_key(session, key, kitty);
        return;
    }
    if session.overlay {
        apply_overlay_key(session, transcript, key, kitty);
        return;
    }
    if live.expanded().is_some() {
        apply_expand_key(live, key, kitty);
        return;
    }
    let viewport = screen == crate::Screen::Fullscreen;
    if viewport && session.viewport.search_text().is_some() {
        apply_search_key(session, transcript, key);
        return;
    }
    let action = resolve_in(key, kitty, key_owners(viewport));
    if let Some(action) = action
        && apply_viewport_action(session, transcript, action, screen)
    {
        return;
    }
    apply_composer_action(session, transcript, live, entries, diagrams, key, action);
    session.update_popup();
}

/// Expand-view keys: the view owns the input. Esc, q, or the same Ctrl+O that
/// opened it collapse it; the transcript page keys scroll the card's rows and
/// Up/Down scroll one row; everything else is swallowed so no typed text
/// reaches the composer behind the view.
fn apply_expand_key(live: &mut Live, key: crate::keys::Key, kitty: bool) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _)
        | (KeyCode::Char('q'), KeyModifiers::NONE)
        | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
            live.close_expand();
            return;
        }
        _ => {}
    }
    let Some(view) = live.expanded() else {
        return;
    };
    match (key.code, key.modifiers) {
        (KeyCode::Up, KeyModifiers::NONE) => view.scroll_up(),
        (KeyCode::Down, KeyModifiers::NONE) => view.scroll_down(),
        _ => match resolve_in(key, kitty, &[Owner::Transcript]) {
            Some(Action::TranscriptPageUp) => view.page_up(),
            Some(Action::TranscriptPageDown) => view.page_down(),
            Some(Action::TranscriptJumpLatest) => view.jump_latest(),
            _ => {}
        },
    }
}

fn apply_composer_action(
    session: &mut Session,
    transcript: &Transcript,
    live: &mut Live,
    entries: &[dal_core::EntryView],
    diagrams: crate::diagram::DiagramSettings,
    key: crate::keys::Key,
    action: Option<Action>,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match action {
        Some(Action::QuitEmptyComposer) if session.composer.is_empty() => session.quit = true,
        Some(Action::QuitEmptyComposer) => {
            session.viewport.clear_selection();
            session.composer.delete();
        }
        Some(Action::Submit) => {
            session.viewport.clear_selection();
            let submitted = session.composer.text().to_owned();
            session.submit_line(&submitted);
        }
        Some(Action::CloseOverlayOrInterrupt | Action::Interrupt) => session.interrupt(),
        Some(Action::CopySelection) => {
            if let Some(selection) = session.viewport.selected_text(transcript) {
                session.pending_copies.push(selection.into_boxed_str());
            }
        }
        Some(Action::Newline) => {
            session.viewport.clear_selection();
            session.composer.insert("\n");
        }
        Some(Action::AcceptCycleCompletion) => {
            session.viewport.clear_selection();
            if let Some(candidate) = session.candidates.first() {
                let completed = crate::popup::accept(session.composer.text(), candidate);
                session.composer.set(&completed);
            } else {
                session.composer.insert("  ");
            }
        }
        Some(Action::Help) => session.help = Some(HelpUi::default()),
        // Ctrl+Z parks the terminal through the same restore the exit path
        // uses, then raises SIGTSTP; the loop's resume path re-enters the
        // recorded modes and repaints. Windows leaves the key unbound.
        Some(Action::Suspend) => session.suspend = true,
        Some(Action::ExpandCollapse) => {
            let opened = entries.iter().rev().find_map(|entry| {
                crate::render::expanded_card(entry, diagrams).map(|card| {
                    let blob_ids = crate::render::expanded_blob_ids(entry, card);
                    (card, blob_ids)
                })
            });
            if let Some((card, blob_ids)) = opened {
                let needs_load = !blob_ids.is_empty();
                live.open_expand(card, blob_ids);
                if needs_load && let Some(view) = live.expanded() {
                    session
                        .front
                        .push(FrontRequest::LoadBlobs(view.blob_sink()));
                }
            }
        }
        Some(action) if apply_edit_action(&mut session.composer, action) => {
            session.viewport.clear_selection();
        }
        Some(Action::HistoryPrev) if session.composer.on_first_line() => {
            session.viewport.clear_selection();
            session.composer.history_prev();
        }
        Some(Action::HistoryPrev) => {
            session.viewport.clear_selection();
            session.composer.move_up();
        }
        Some(Action::HistoryNext) if session.composer.on_last_line() => {
            session.viewport.clear_selection();
            session.composer.history_next();
        }
        Some(Action::HistoryNext) => {
            session.viewport.clear_selection();
            session.composer.move_down();
        }
        None if key.code == KeyCode::Backspace => {
            session.viewport.clear_selection();
            session.composer.backspace();
        }
        None if key.code == KeyCode::Delete => {
            session.viewport.clear_selection();
            session.composer.delete();
        }
        None if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT => {
            session.viewport.clear_selection();
            if let KeyCode::Char(character) = key.code {
                session.composer.insert(character.encode_utf8(&mut [0; 4]));
            }
        }
        _ => {}
    }
}

/// Picker keys: Esc dismisses, Enter activates the focused option, and
/// arrows plus plain characters drive selection and filter text.
fn apply_picker_key(session: &mut Session, key: crate::keys::Key) {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Esc => session.picker = None,
        KeyCode::Enter => {
            match session
                .picker
                .as_mut()
                .and_then(PickerUi::activate_selected)
            {
                Some(PickerAction::Command(command)) => {
                    session.pending_commands.push(command);
                    session.picker = None;
                }
                Some(PickerAction::ConfirmLogoutAll) => {
                    session.signin = Some(SignIn::confirm_all());
                    session.picker = None;
                }
                Some(PickerAction::SetDiagrams { enabled, save }) => {
                    session.pending_diagram_settings.push((enabled, save));
                }
                // The failed model picker's instruction row activates
                // nothing and keeps the failure surface open.
                Some(PickerAction::Noop) => {}
                Some(_) | None => session.picker = None,
            }
        }
        KeyCode::Up => {
            if let Some(picker) = &mut session.picker {
                picker.move_selection(false);
            }
        }
        KeyCode::Down => {
            if let Some(picker) = &mut session.picker {
                picker.move_selection(true);
            }
        }
        KeyCode::Backspace => {
            if let Some(picker) = &mut session.picker {
                picker.remove_filter_char();
            }
        }
        KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
            apply_model_retry_key(session);
        }
        KeyCode::Char(' ')
            if key.modifiers == KeyModifiers::NONE
                && session
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.filter().is_empty()) =>
        {
            if let Some(picker) = &mut session.picker {
                // picker.rs owns the contract: toggle_selected is a no-op on a
                // single picker, so unmodified Space never joins the filter on
                // an empty query and never invents a check where none exists.
                picker.toggle_selected();
            }
        }
        KeyCode::Char(character)
            if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT =>
        {
            if let Some(picker) = &mut session.picker {
                picker.add_filter_char(character);
            }
        }
        _ => {}
    }
}

/// Suspends the process on POSIX:
/// writes the recorded restore bytes, leaves raw mode, and raises SIGTSTP
/// through the terminal seam. `raise_tstp` is a no-op on Windows, so the
/// key stays inert there instead of parking the terminal permanently. On
/// return (SIGCONT) raw mode re-enters, the same modes push again in the
/// same order, and the repaint that follows rewrites the live block at the
/// measured caret row.
fn suspend_and_continue(
    io: &dyn TermIo,
    state: &Arc<Mutex<TermState>>,
    opts: &TuiOptions,
    theme: &ResolvedTheme,
    painter: &mut Painter,
    surfaces: &mut Surfaces,
) -> Result<(), TuiError> {
    let saved = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let restore = crate::term::restore_bytes(&saved);
    io.write(&restore)
        .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
    io.disable_raw();
    io.raise_tstp();
    reenter_modes(io, state, opts, &saved)
        .map_err(|message| crate::term::terminal_error(&message))?;
    // Repaint in place: the restore left the screen outside the painter's
    // damage model, so this frame rewrites the whole live block. A diff paint
    // that finds every row unchanged would write nothing and leave whatever
    // the shell printed on screen (and, fullscreen, the blank alternate
    // screen the re-entry just made), so the repaint drops the baseline first.
    repaint_live_region(io, state, opts, theme, painter, surfaces)
}

/// Ctrl+L and the return from a suspend: clear the live region by repainting
/// it in full. The live block holds only transient rows, so a whole-block
/// repaint is cheap, and it also repairs any rows a bidi payload mangled or a
/// foreign process left behind.
fn repaint_live_region(
    io: &dyn TermIo,
    state: &Arc<Mutex<TermState>>,
    opts: &TuiOptions,
    theme: &ResolvedTheme,
    painter: &mut Painter,
    surfaces: &mut Surfaces,
) -> Result<(), TuiError> {
    painter.invalidate();
    surfaces.paint(io, state, painter, opts, theme)
}

/// Re-enters raw mode and pushes every recorded terminal mode again, in
/// the startup order, after the terminal was restored for a child process.
fn reenter_modes(
    io: &dyn TermIo,
    state: &Arc<Mutex<TermState>>,
    opts: &TuiOptions,
    saved: &TermState,
) -> Result<(), String> {
    io.enable_raw()
        .map_err(|error| format!("cannot re-enter raw mode: {error}"))?;
    {
        let mut modes = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        modes.set_raw(true);
        modes.set_paste(true);
    }
    io.write(b"\x1b[?2004h")
        .map_err(|error| format!("cannot re-enter bracketed paste: {error}"))?;
    if opts.screen == crate::Screen::Fullscreen {
        io.write(b"\x1b[?1049h\x1b[?25l")
            .map_err(|error| format!("cannot re-enter the alternate screen: {error}"))?;
    }
    if saved.kitty() {
        io.write(b"\x1b[>1u")
            .map_err(|error| format!("cannot re-enter kitty keyboard: {error}"))?;
    }
    if saved.grapheme() {
        io.write(b"\x1b[?2027h")
            .map_err(|error| format!("cannot re-enter grapheme mode: {error}"))?;
    }
    Ok(())
}

/// `r` in the failed model picker requests one retry through the real model
/// source at the next tick; in any other picker it stays a filter character.
fn apply_model_retry_key(session: &mut Session) {
    let Some(picker) = session.picker.as_mut() else {
        return;
    };
    if !picker.is_load_failed() {
        picker.add_filter_char('r');
        return;
    }
    picker.begin_retry();
    session.pending_model_retry = true;
}

/// Runs the retry the failed model picker asked for: one call through the
/// real model source, the query/filter kept, and either a rebuilt picker or
/// the failure surface restored. The retry runs only while the failed
/// picker is still open, never after a dismissal or in another surface.
fn poll_model_retry<M>(session: &mut Session, model_source: &mut M)
where
    M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
{
    if !std::mem::take(&mut session.pending_model_retry) {
        return;
    }
    let Some(picker) = session.picker.as_ref() else {
        return;
    };
    if !picker.is_load_failed() {
        return;
    }
    let filter = picker.filter().to_owned();
    match model_source() {
        Ok(models) => session.picker = Some(crate::picker::model_picker(&models, &filter)),
        Err(_) => {
            if let Some(picker) = &mut session.picker {
                picker.fail_retry();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::{Session, apply_event_at, poll_model_retry};

    /// A resolution that names `core` and the default deny answer: the shape
    /// every deadline resolution has, so no cause can be read off it.
    #[test]
    fn resolution_notice_follows_the_cause_and_never_guesses_one() {
        use dal_core::{Answer, ClientId, ResolutionCause};

        let notice = |cause| {
            super::resolution_notice(cause, &ClientId::new("core"), &Answer::Decline, "ship it")
        };
        let default_word = crate::copy::answer_word(&Answer::Decline);
        let causes = [
            Some(ResolutionCause::Answered),
            Some(ResolutionCause::TimedOut),
            Some(ResolutionCause::Cancelled),
            Some(ResolutionCause::Withdrawn),
            Some(ResolutionCause::NoController),
            None,
        ];
        let notices: Vec<String> = causes.into_iter().map(notice).collect();

        let distinct: std::collections::HashSet<&String> = notices.iter().collect();
        assert_eq!(
            distinct.len(),
            causes.len(),
            "every cause, and a missing one, reads differently: {notices:?}"
        );
        assert!(notices.iter().all(|text| text.contains("ship it")));
        for cause in [ResolutionCause::TimedOut, ResolutionCause::NoController] {
            assert!(
                notice(Some(cause)).contains(&default_word),
                "{cause:?} names the default dal used"
            );
        }
        for cause in [ResolutionCause::Cancelled, ResolutionCause::Withdrawn] {
            assert!(
                !notice(Some(cause)).contains(&default_word),
                "{cause:?} answered nothing, so it names no answer"
            );
        }
        let legacy = notice(None);
        assert!(
            notices[..5].iter().all(|text| *text != legacy),
            "a missing cause must not borrow a known cause's wording: {legacy}"
        );
    }

    /// Applies one key to a fresh dialog, live region, and transcript on the
    /// inline screen.
    fn apply_key(session: &mut Session, key: crate::keys::Key) {
        use crate::keys::InputEvent;

        apply_event_at(
            session,
            &mut crate::dialog::DialogUi::default(),
            &crate::transcript::Transcript::default(),
            &mut crate::live::Live::default(),
            &[],
            crate::diagram::DiagramSettings::default(),
            InputEvent::Key(key),
            false,
            crate::Screen::Inline,
            80,
            24,
        );
    }

    #[test]
    fn the_exit_draft_frame_input_names_the_focused_verb() {
        let mut session = Session {
            composer: crate::composer::Composer::default(),
            ..Session::default()
        };
        session.composer.set("draft");
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        let confirm = session.dialog.as_ref().expect("the question opened");
        assert_eq!(
            Some(confirm.focused()),
            Some(crate::dialog::DraftChoice::Keep),
            "the frame input names the focused verb for the highlight"
        );
        let no_dialog = Session::default();
        assert!(
            no_dialog.dialog.is_none(),
            "no question, no exit-draft rows"
        );
    }

    #[test]
    fn chooser_reply_is_not_rendered_as_debug_text() {
        use dal_core::Chooser;
        // Every chooser opens its picker upstream in fold_submit_reply; at this
        // layer each one must stay rowless so no chooser ever leaks debug rows.
        for chooser in [
            Chooser::Tree,
            Chooser::ForkPoint,
            Chooser::Model,
            Chooser::Settings,
            Chooser::Login,
            Chooser::Logout,
            Chooser::Thinking,
            Chooser::ScopedModels,
            Chooser::Session,
        ] {
            let mut session = Session::default();
            assert_eq!(
                session.accept_reply(dal_core::Reply::Choose {
                    chooser,
                    filter: "".into(),
                }),
                [] as [String; 0],
                "{chooser:?} must render no rows"
            );
        }
    }

    #[test]
    fn unhandled_front_actions_render_one_debug_row() {
        let mut session = Session::default();
        let rows = session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Import {
            path: std::path::PathBuf::from("/tmp/session.json"),
        }));
        assert_eq!(
            rows.len(),
            1,
            "unhandled actions stay visible, never silent"
        );
        assert!(
            rows[0].contains("Import"),
            "debug row names its action: {rows:?}"
        );
        assert!(session.picker.is_none());
        assert!(session.front.is_empty());
    }

    #[test]
    fn picker_arrow_enter_selects_and_escape_or_control_c_cancels() {
        use crate::keys::Key;
        use crate::picker::{PickerAction, PickerOption, PickerUi};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            picker: Some(PickerUi::new(
                "Pick a model",
                "",
                vec![
                    PickerOption {
                        label: "first".to_owned(),
                        action: PickerAction::Command(dal_core::Command::Run {
                            name: "model".into(),
                            args: "openai/first".into(),
                            expected: None,
                        }),
                    },
                    PickerOption {
                        label: "second".to_owned(),
                        action: PickerAction::Command(dal_core::Command::Run {
                            name: "model".into(),
                            args: "openai/second".into(),
                            expected: None,
                        }),
                    },
                ],
            )),
            ..Session::default()
        };
        apply_key(&mut session, Key::new(KeyCode::Down, KeyModifiers::NONE));
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(&session.pending_commands[..], [dal_core::Command::Run { args, .. }] if args.as_ref() == "openai/second")
        );
        assert!(session.picker.is_none());

        session.picker = Some(PickerUi::new("Pick", "", Vec::new()));
        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.picker.is_none());
        assert_eq!(session.pending_commands.len(), 1);

        session.picker = Some(PickerUi::new("Pick", "", Vec::new()));
        session.active_turn = Some(dal_core::TurnId::new(NonZeroU64::MIN));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(session.picker.is_none());
        assert_eq!(session.pending_commands.len(), 1);
    }

    /// A committed id the model list no longer names is a visible, checked row.
    /// Unchecking another row leaves it committed; unchecking it too submits
    /// the empty list that clears the scope.
    #[test]
    fn scoped_picker_keeps_a_missing_committed_id_until_it_is_unchecked() {
        let rows = vec![crate::picker::ScopedModelRow {
            id: "openai/gpt-6".into(),
            label: "GPT 6".into(),
        }];
        let mut picker = crate::picker::scoped_models_picker(
            rows,
            &["openai/gpt-6".into(), "anthropic/vanished".into()],
            "",
        );
        assert!(picker.is_checked(0));
        assert!(picker.is_checked(1), "the missing id is a checked row");
        picker.toggle_selected();
        let action = picker.activate_selected().expect("multi submits");
        match action {
            crate::picker::PickerAction::Command(dal_core::Command::SetScopedModels(ids)) => {
                assert!(
                    ids.iter().map(|id| id.as_ref()).eq(["anthropic/vanished"]),
                    "the untouched missing id is still committed, got {ids:?}"
                );
            }
            other => panic!("expected SetScopedModels, got {other:?}"),
        }
        picker.move_selection(true);
        picker.toggle_selected();
        let action = picker.activate_selected().expect("multi submits");
        assert!(
            matches!(
                action,
                crate::picker::PickerAction::Command(dal_core::Command::SetScopedModels(ids))
                    if ids.is_empty()
            ),
            "unchecking the missing id clears the scope"
        );
    }

    #[test]
    fn settings_picker_updates_local_state_and_queues_config_saves() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            picker: Some(crate::picker::settings_picker(false, "")),
            ..Session::default()
        };
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            session.pending_diagram_settings,
            [(true, dal_core::command::Save::SessionOnly)]
        );
        assert_eq!(session.pending_commands, []);
        assert!(session.picker.is_some());

        session
            .picker
            .as_mut()
            .expect("settings picker")
            .move_selection(true);
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            session.pending_diagram_settings,
            [
                (true, dal_core::command::Save::SessionOnly),
                (true, dal_core::command::Save::SessionAndDefault),
            ]
        );
    }

    #[test]
    fn quit_with_draft_requires_confirmation() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut composer = crate::composer::Composer::default();
        composer.set("rewrite the query");
        let mut session = Session {
            composer,
            ..Session::default()
        };
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        assert!(
            session.dialog.is_some(),
            "a non-empty draft asks before quitting"
        );
        assert!(!session.quit);

        // A blind Enter keeps the draft: the conservative verb holds focus.
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(session.dialog.is_none());
        assert!(!session.quit);
        assert_eq!(session.composer.text(), "rewrite the query");

        // Esc answers Keep too, so a panicked Esc never throws work away.
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.dialog.is_none());
        assert!(!session.quit);
        assert_eq!(session.composer.text(), "rewrite the query");

        // Tab and Shift+Tab move the focus without answering; Enter on the
        // focused verb activates it.
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(
            session.dialog.is_some(),
            "focus moves never answer the question"
        );
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(session.quit);
        assert_eq!(session.composer.text(), "");
    }

    /// The committed quit-confirmation letters still work beside Tab, Enter
    /// and Esc: `n`/`N` close the question and keep the draft, `y`/`Y` discard
    /// it and quit, and a key that is none of those leaves the question open.
    #[test]
    fn draft_confirm_keeps_the_committed_y_and_n_answers() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let ask = || {
            let mut composer = crate::composer::Composer::default();
            composer.set("rewrite the query");
            let mut session = Session {
                composer,
                ..Session::default()
            };
            session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
            assert!(session.dialog.is_some(), "a non-empty draft asks first");
            session
        };
        for letter in ['n', 'N'] {
            let mut session = ask();
            apply_key(
                &mut session,
                Key::new(KeyCode::Char(letter), KeyModifiers::NONE),
            );
            assert!(session.dialog.is_none(), "{letter:?} closes the question");
            assert!(!session.quit, "{letter:?} keeps the session");
            assert_eq!(session.composer.text(), "rewrite the query");
        }
        for letter in ['y', 'Y'] {
            let mut session = ask();
            apply_key(
                &mut session,
                Key::new(KeyCode::Char(letter), KeyModifiers::NONE),
            );
            assert!(session.quit, "{letter:?} quits");
            assert!(session.dialog.is_none());
            assert_eq!(session.composer.text(), "", "{letter:?} discards the draft");
        }
        let mut session = ask();
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(session.dialog.is_some(), "an unrelated key leaves it open");
        assert!(!session.quit);
        assert_eq!(session.composer.text(), "rewrite the query");
    }

    #[test]
    fn quit_command_routes_through_host_registry() {
        let mut session = Session::default();
        session.submit_line("/quit");
        assert!(
            matches!(&session.pending_commands[0], dal_core::Command::Run { name, args, .. }
            if name.as_ref() == "quit" && args.is_empty())
        );
        assert!(!session.quit);
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        assert!(session.quit);
    }

    #[test]
    fn text_typed_while_a_turn_runs_steers_that_turn() {
        let turn = dal_core::TurnId::new(NonZeroU64::MIN);
        let mut session = Session {
            active_turn: Some(turn),
            ..Session::default()
        };
        session.submit_line("also check the tests");
        assert!(matches!(
            &session.pending_commands[..],
            [dal_core::Command::Steer { turn: steered, content }]
                if *steered == turn && content.len() == 1
        ));
        assert!(session.composer.is_empty());
        assert!(
            session
                .accept_reply(dal_core::Reply::Queued { turn: None })
                .is_empty(),
            "the live block shows the queue; no transcript row is written"
        );
    }

    #[test]
    fn slash_commands_typed_while_a_turn_runs_stay_commands() {
        let mut session = Session {
            active_turn: Some(dal_core::TurnId::new(NonZeroU64::MIN)),
            ..Session::default()
        };
        session.submit_line("/session");
        assert!(matches!(
            &session.pending_commands[..],
            [dal_core::Command::Run { name, .. }] if name.as_ref() == "session"
        ));
    }

    #[test]
    fn tab_completes_a_skill_token_in_place_and_the_slash_list_hides_skills() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let commands: Vec<dal_core::CommandSpec> = ["skill", "skill:find-anything", "skill:review"]
            .into_iter()
            .map(|name| dal_core::CommandSpec {
                name: dal_core::CommandName::parse(name).unwrap(),
                summary: format!("{name} summary").into(),
                args_hint: None,
            })
            .collect();
        let mut session = Session {
            commands: commands.into(),
            ..Session::default()
        };
        session.composer.set("/s");
        session.update_popup();
        assert_eq!(session.popup, ["/skill · skill summary"]);
        session.composer.set("check with $fi");
        session.update_popup();
        assert_eq!(
            session.popup,
            ["$find-anything · skill:find-anything summary"]
        );
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "check with $find-anything ");
        session.submit_line("check with $find-anything ");
        assert!(matches!(
            &session.pending_commands[..],
            [dal_core::Command::Prompt { .. }]
        ));
    }

    #[test]
    fn copy_reply_queues_the_text_for_the_clipboard_without_debug_rows() {
        let mut session = Session::default();
        let rows = session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::CopyReply {
            text: "the reply".into(),
        }));
        assert_eq!(rows.len(), 0);
        assert_eq!(session.pending_copies, vec!["the reply".into()]);
    }

    #[test]
    fn help_consumes_keys_and_paste_without_changing_the_draft() {
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session::default();
        session.composer.set("kept draft");
        apply_key(&mut session, Key::new(KeyCode::F(1), KeyModifiers::NONE));
        for key in [
            Key::new(KeyCode::Char('i'), KeyModifiers::NONE),
            Key::new(KeyCode::Enter, KeyModifiers::NONE),
            Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Key::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            Key::new(KeyCode::PageDown, KeyModifiers::NONE),
        ] {
            apply_key(&mut session, key);
        }
        apply_event_at(
            &mut session,
            &mut crate::dialog::DialogUi::default(),
            &crate::transcript::Transcript::default(),
            &mut crate::live::Live::default(),
            &[],
            crate::diagram::DiagramSettings::default(),
            InputEvent::Paste(b"ignored paste".to_vec()),
            false,
            crate::Screen::Inline,
            80,
            24,
        );
        assert!(
            session.help.is_some(),
            "typing and pasting never dismiss the help"
        );
        assert_eq!(session.composer.text(), "kept draft");
        assert_eq!(session.pending_commands, []);
        assert_eq!(session.pending_answers, []);

        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.help.is_none());
        assert_eq!(session.composer.text(), "kept draft");
    }

    #[test]
    fn control_c_closes_help_before_interrupting_the_turn() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            active_turn: Some(dal_core::TurnId::new(NonZeroU64::MIN)),
            ..Session::default()
        };
        apply_key(&mut session, Key::new(KeyCode::F(1), KeyModifiers::NONE));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(session.help.is_none());
        assert_eq!(
            session.pending_commands,
            [],
            "closing the help never interrupts the turn"
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(matches!(
            &session.pending_commands[..],
            [dal_core::Command::Cancel { .. }]
        ));
    }

    #[test]
    fn submitted_prompts_queue_typed_commands() {
        let mut session = Session::default();
        session.submit_line("rewrite the query");
        assert_eq!(session.pending_commands.len(), 1);
        assert!(matches!(
            &session.pending_commands[0],
            dal_core::Command::Prompt { .. }
        ));
        if let dal_core::Command::Prompt { expect, content } = &session.pending_commands[0] {
            assert_eq!(expect, &dal_core::Expect::Idle);
            assert_eq!(content.len(), 1);
        }
        assert_eq!(session.composer.text(), "");
    }

    #[test]
    fn r_in_the_failed_model_picker_requests_one_retry_and_keeps_the_filter() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            picker: Some(crate::picker::model_picker_failed("gpt")),
            ..Session::default()
        };
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert!(session.pending_model_retry);
        assert!(
            session.picker.is_some(),
            "the failure surface stays open while the retry runs"
        );
        assert_eq!(
            session.picker.as_ref().map(|picker| picker.title.as_str()),
            Some(crate::copy::ids::MODEL_PICKER_FETCHING)
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gpt")
        );
        assert_eq!(session.pending_commands, []);

        // Other keys still edit the query/filter while the retry is pending.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('4'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gpt4")
        );
        assert!(
            session.pending_model_retry,
            "the single r request stays pending until the tick runs it"
        );
    }

    #[test]
    fn model_retry_rebuilds_the_picker_through_the_real_source() {
        let mut session = Session {
            picker: Some(crate::picker::model_picker_failed("gpt")),
            ..Session::default()
        };
        let attempts = std::cell::Cell::new(0_u32);
        let mut source = || -> Result<Vec<crate::picker::ModelOption>, crate::TuiError> {
            attempts.set(attempts.get() + 1);
            Err(crate::TuiError::Terminal("the network is down".into()))
        };
        session.pending_model_retry = true;
        poll_model_retry(&mut session, &mut source);
        assert_eq!(attempts.get(), 1);
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(crate::picker::PickerUi::is_load_failed),
            "a failed retry keeps the failure surface"
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gpt"),
            "the query/filter survives a failed retry"
        );

        session.pending_model_retry = true;
        poll_model_retry(&mut session, &mut source);
        assert_eq!(attempts.get(), 2, "r runs one real attempt each press");

        let models = vec![crate::picker::ModelOption {
            label: "GPT 6 · openai-chat/gpt-6".to_owned(),
            command: dal_core::Command::SetModel {
                model: dal_core::ModelRoute::Harness {
                    id: "dalgon/normal".into(),
                },
                save: dal_core::command::Save::SessionAndDefault,
            },
            scoped_id: None,
        }];
        let mut good_source =
            || -> Result<Vec<crate::picker::ModelOption>, crate::TuiError> { Ok(models.clone()) };
        session.pending_model_retry = true;
        poll_model_retry(&mut session, &mut good_source);
        let picker = session
            .picker
            .as_ref()
            .expect("a successful retry rebuilds the picker");
        assert!(!picker.is_load_failed());
        assert_eq!(
            picker.filter(),
            "gpt",
            "the query/filter survives the retry"
        );
        assert_eq!(picker.visible_len(), 1);
    }

    #[test]
    fn model_retry_never_runs_outside_the_failed_picker() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        // Typing r in the composer is plain text and queues no retry.
        let mut session = Session::default();
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert_eq!(session.composer.text(), "r");
        assert!(!session.pending_model_retry);
        let mut unreachable = || -> Result<Vec<crate::picker::ModelOption>, crate::TuiError> {
            panic!("the model source must not run without a failed picker")
        };
        poll_model_retry(&mut session, &mut unreachable);
        assert_eq!(session.composer.text(), "r", "the composer draft is intact");

        // r in a healthy model picker filters, it never retries.
        let models = vec![crate::picker::ModelOption {
            label: "GPT 6 · openai-chat/gpt-6".to_owned(),
            command: dal_core::Command::SetModel {
                model: dal_core::ModelRoute::Harness {
                    id: "dalgon/normal".into(),
                },
                save: dal_core::command::Save::SessionAndDefault,
            },
            scoped_id: None,
        }];
        session.picker = Some(crate::picker::model_picker(&models, ""));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert!(!session.pending_model_retry);
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("r")
        );
        poll_model_retry(&mut session, &mut unreachable);

        // A stale flag dies with the picker: Esc dismisses, nothing retries.
        session.picker = Some(crate::picker::model_picker_failed(""));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert!(session.pending_model_retry);
        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.picker.is_none());
        poll_model_retry(&mut session, &mut unreachable);
    }

    #[test]
    fn failed_model_picker_enter_on_a_typed_id_runs_model_and_keeps_no_retry() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            picker: Some(crate::picker::model_picker_failed("")),
            ..Session::default()
        };
        for character in ['g', 'p', 't'] {
            apply_key(
                &mut session,
                Key::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(
                &session.pending_commands[..],
                [dal_core::Command::Run { name, args, .. }]
                    if name.as_ref() == "model" && args.as_ref() == "gpt"
            ),
            "a typed id is accepted straight from the failure surface"
        );
        assert!(session.picker.is_none());
        assert!(!session.pending_model_retry);
    }

    mod fullscreen_selection {
        use std::num::NonZeroU64;

        use super::{Session, apply_event_at};
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crate::transcript::Transcript;
        use crossterm::event::{KeyCode, KeyModifiers};

        /// Three settled rows, like the assistant's last three lines.
        fn transcript() -> Transcript {
            let mut transcript = Transcript::default();
            transcript.commit("entry-1", &["alpha row".to_owned(), "beta row".to_owned()]);
            transcript.commit("entry-2", &["gamma row".to_owned()]);
            transcript
        }

        /// Applies one key on the fullscreen screen.
        fn apply_full(
            session: &mut Session,
            dialog: &mut DialogUi,
            transcript: &Transcript,
            key: Key,
        ) {
            apply_event_at(
                session,
                dialog,
                transcript,
                &mut crate::live::Live::default(),
                &[],
                crate::diagram::DiagramSettings::default(),
                InputEvent::Key(key),
                false,
                crate::Screen::Fullscreen,
                80,
                24,
            );
        }

        fn shift(code: KeyCode) -> Key {
            Key::new(code, KeyModifiers::SHIFT)
        }

        fn control_c() -> Key {
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
        }

        #[test]
        fn shift_arrows_build_the_selection_and_ctrl_c_copies_without_interrupting() {
            let mut session = Session {
                active_turn: Some(dal_core::TurnId::new(NonZeroU64::MIN)),
                ..Session::default()
            };
            let transcript = transcript();
            let mut dialog = DialogUi::default();

            apply_full(&mut session, &mut dialog, &transcript, shift(KeyCode::Up));
            apply_full(&mut session, &mut dialog, &transcript, shift(KeyCode::Up));
            assert!(session.viewport.selection().is_some());

            // Busy boundary: a running turn is not cancelled by copying.
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert_eq!(
                session
                    .pending_copies
                    .iter()
                    .map(std::convert::AsRef::as_ref)
                    .collect::<Vec<_>>(),
                ["\nbeta row\ngamma row"]
            );
            assert_eq!(
                session.pending_commands,
                [],
                "copying the selection never submits the cancellation"
            );
            assert_eq!(session.composer.text(), "", "the draft is untouched");

            // Typing returns focus to the composer and collapses the selection.
            apply_full(
                &mut session,
                &mut dialog,
                &transcript,
                Key::new(KeyCode::Char('x'), KeyModifiers::NONE),
            );
            assert_eq!(session.composer.text(), "x");
            assert!(session.viewport.selection().is_none());

            // Without a selection Ctrl+C interrupts the running turn again.
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert!(matches!(
                &session.pending_commands[..],
                [dal_core::Command::Cancel { .. }]
            ));
        }

        #[test]
        fn inline_mode_keeps_no_selection_state() {
            let mut session = Session {
                active_turn: Some(dal_core::TurnId::new(NonZeroU64::MIN)),
                ..Session::default()
            };
            let transcript = transcript();
            let mut dialog = DialogUi::default();
            for code in [KeyCode::Up, KeyCode::Left, KeyCode::Down, KeyCode::Right] {
                apply_event_at(
                    &mut session,
                    &mut dialog,
                    &transcript,
                    &mut crate::live::Live::default(),
                    &[],
                    crate::diagram::DiagramSettings::default(),
                    InputEvent::Key(shift(code)),
                    false,
                    crate::Screen::Inline,
                    80,
                    24,
                );
            }
            assert!(
                session.viewport.selection().is_none(),
                "inline leaves selection to the terminal's native mechanism"
            );
            assert_eq!(
                session.composer.text(),
                "",
                "no stray text reaches the composer"
            );
            apply_event_at(
                &mut session,
                &mut dialog,
                &transcript,
                &mut crate::live::Live::default(),
                &[],
                crate::diagram::DiagramSettings::default(),
                InputEvent::Key(control_c()),
                false,
                crate::Screen::Inline,
                80,
                24,
            );
            assert_eq!(session.pending_copies, []);
            assert!(matches!(
                &session.pending_commands[..],
                [dal_core::Command::Cancel { .. }]
            ));
        }

        #[test]
        fn a_collapsed_selection_falls_through_to_interrupt() {
            let mut session = Session {
                active_turn: Some(dal_core::TurnId::new(NonZeroU64::MIN)),
                ..Session::default()
            };
            let transcript = transcript();
            let mut dialog = DialogUi::default();
            // Shift+Up then Shift+Down walks the focus back onto the anchor.
            apply_full(&mut session, &mut dialog, &transcript, shift(KeyCode::Up));
            apply_full(&mut session, &mut dialog, &transcript, shift(KeyCode::Down));
            assert_eq!(
                session
                    .viewport
                    .selection()
                    .map(|selection| selection.focus),
                session
                    .viewport
                    .selection()
                    .map(|selection| selection.anchor),
                "the selection collapsed"
            );
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert_eq!(
                session.pending_copies,
                [],
                "an empty selection copies nothing"
            );
            assert!(matches!(
                &session.pending_commands[..],
                [dal_core::Command::Cancel { .. }]
            ));
        }

        #[test]
        fn help_and_picker_close_before_the_selection_copies() {
            let mut session = Session::default();
            let transcript = transcript();
            let mut dialog = DialogUi::default();
            apply_full(&mut session, &mut dialog, &transcript, shift(KeyCode::Up));
            let selection = session.viewport.selection();

            // The F1 help overlay closes first; the selection survives it.
            apply_full(
                &mut session,
                &mut dialog,
                &transcript,
                Key::new(KeyCode::F(1), KeyModifiers::NONE),
            );
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert!(session.help.is_none());
            assert_eq!(session.pending_copies, []);
            assert_eq!(session.viewport.selection(), selection, "copying waited");

            // The next Ctrl+C finds the help gone and copies the selection.
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert_eq!(session.pending_copies.len(), 1);
            assert_eq!(session.pending_commands, []);

            // A picker closes before copying too.
            session.picker = Some(crate::picker::model_picker_failed(""));
            apply_full(&mut session, &mut dialog, &transcript, control_c());
            assert!(session.picker.is_none());
            assert_eq!(
                session.pending_copies.len(),
                1,
                "the picker closed instead of copying"
            );
        }
    }

    #[test]
    fn floored_surface_drops_blind_keys_until_resized_back() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        fn char_key(c: char) -> Key {
            Key::new(KeyCode::Char(c), KeyModifiers::NONE)
        }

        fn approval(id: dal_core::RequestId) -> dal_core::Request {
            dal_core::Request {
                id,
                turn: None,
                owner: dal_core::Owner::Core,
                question: dal_core::Question::Approval {
                    tool: "exec".into(),
                    preview: dal_core::Preview {
                        title: "command".into(),
                        body: "echo hi".into(),
                        digest: None,
                    },
                    grant: None,
                    call: None,
                },
                timeout: std::time::Duration::from_secs(30),
                default: dal_core::Answer::Decline,
            }
        }

        fn apply_at(
            session: &mut Session,
            dialog: &mut DialogUi,
            event: InputEvent,
            width: u16,
            height: u16,
        ) {
            super::apply_event_at(
                session,
                dialog,
                &crate::transcript::Transcript::default(),
                &mut crate::live::Live::default(),
                &[],
                crate::diagram::DiagramSettings::default(),
                event,
                false,
                crate::Screen::Inline,
                width,
                height,
            );
        }

        assert!(crate::render::below_floor(20, 5));
        assert!(!crate::render::below_floor(80, 24));

        let mut session = Session::default();
        let mut dialog = DialogUi::default();
        let id = dal_core::RequestId::new_v7();
        dialog.opened(approval(id));
        let start = std::time::Instant::now();
        dialog.tick(start);
        dialog.tick(start + crate::dialog::ARM_DELAY);
        assert!(dialog.armed());

        apply_at(
            &mut session,
            &mut dialog,
            InputEvent::Key(char_key('y')),
            20,
            5,
        );
        assert!(
            session.pending_answers.is_empty(),
            "a blind `y` below the floor must not approve"
        );
        assert!(dialog.is_open(), "the unseen question stays open");
        assert!(
            !dialog.awaiting_resolution(),
            "no answer may latch while floored"
        );

        apply_at(
            &mut session,
            &mut dialog,
            InputEvent::Key(char_key('y')),
            80,
            24,
        );
        assert_eq!(session.pending_answers.len(), 1);
        assert!(
            matches!(
                &session.pending_answers[0],
                (answered, dal_core::Answer::Approve) if *answered == id
            ),
            "resize back re-arms the still-open approval: {:?}",
            session.pending_answers
        );

        let mut picker_session = Session {
            picker: Some(crate::picker::model_picker_failed("")),
            ..Session::default()
        };
        let mut no_dialog = DialogUi::default();
        apply_at(
            &mut picker_session,
            &mut no_dialog,
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            20,
            5,
        );
        assert!(
            picker_session.pending_commands.is_empty(),
            "picker Enter below the floor must not activate"
        );
        assert!(
            picker_session.picker.is_some(),
            "the unseen picker stays open"
        );
        apply_at(
            &mut picker_session,
            &mut no_dialog,
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            80,
            24,
        );

        let mut type_session = Session::default();
        let mut type_dialog = DialogUi::default();
        apply_at(
            &mut type_session,
            &mut type_dialog,
            InputEvent::Key(char_key('x')),
            20,
            5,
        );
        assert_eq!(
            type_session.composer.text(),
            "",
            "composer typing below the floor must not mutate the draft"
        );
        apply_at(
            &mut type_session,
            &mut type_dialog,
            InputEvent::Paste(b"x".to_vec()),
            20,
            5,
        );
        assert_eq!(
            type_session.composer.text(),
            "",
            "paste below the floor must not reach the composer"
        );

        let mut interrupt_session = Session {
            picker: Some(crate::picker::model_picker_failed("")),
            ..Session::default()
        };
        let mut interrupt_dialog = DialogUi::default();
        apply_at(
            &mut interrupt_session,
            &mut interrupt_dialog,
            InputEvent::Key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            20,
            5,
        );
        assert!(
            interrupt_session.picker.is_none(),
            "Ctrl+C stays the interrupt path while floored"
        );
    }
}

#[cfg(test)]
mod exit_line_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dal_core::{SessionId, Workspace};

    use crate::term::TermIo;

    const SESSION: &str = "018f0f62-3b00-7000-8000-000000000001";

    /// Keeps every write, so the exit line can be compared byte for byte.
    #[derive(Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl TermIo for Capture {
        fn enable_raw(&self) -> std::io::Result<()> {
            Ok(())
        }

        fn disable_raw(&self) {}

        fn size(&self) -> std::io::Result<(u16, u16)> {
            Ok((80, 24))
        }

        fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
            let mut buffer = self.0.lock().expect("capture lock");
            buffer.extend_from_slice(bytes);
            Ok(())
        }

        fn read(&self, _timeout: Duration) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn raise_tstp(&self) {}
    }

    /// Writes the saved-session exit line for `name` and returns the bytes.
    fn exit_output(name: Option<&str>) -> String {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let root = std::env::temp_dir();
        let workspace = Workspace::new(root).expect("absolute workspace");
        let opts = crate::TuiOptions {
            session: dal_agent::SessionRef::Ephemeral { workspace },
            screen: crate::Screen::Inline,
            theme_request: crate::ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "true".into(),
            color: crate::ColorMode::Never,
            binary: "dal",
            env: crate::EnvFacts::default(),
            rt: runtime.handle().clone(),
        };
        let id = SessionId::parse(SESSION).expect("fixed v7 id parses");
        let exit = crate::TuiExit {
            session: Some(id),
            name: name.map(Box::from),
            messages: 1,
            ephemeral: false,
        };
        let io = Capture::default();
        let written = super::write_exit(&io, &opts, id, &exit, Vec::new());
        written.expect("exit line writes");
        let bytes = std::mem::take(&mut *io.0.lock().expect("capture lock"));
        String::from_utf8(bytes).expect("exit line is UTF-8")
    }

    /// Returns the `-r` argument the exit line offers: the text between the
    /// `-r` flag and the end of the offer. Assertions see the resume argument,
    /// never the surrounding sentence.
    fn resume_arg(name: Option<&str>) -> String {
        const FLAG: &str = " -r ";
        const TAIL: &str = " to resume";
        let line = exit_output(name);
        let start = line.rfind(FLAG).expect("exit line offers -r") + FLAG.len();
        let rest = &line[start..];
        let end = rest.find(TAIL).expect("offer ends after the argument");
        rest[..end].to_owned()
    }

    /// A bare name resumes as itself.
    #[test]
    fn exit_line_offers_a_bare_name_itself() {
        assert_eq!(resume_arg(Some("demo")), "demo");
    }

    /// A name with a space resumes as one double-quoted argument.
    #[test]
    fn exit_line_quotes_a_spaced_name_into_one_argument() {
        assert_eq!(resume_arg(Some("my session")), "\"my session\"");
    }

    /// A name the shell would expand resumes by session id instead.
    #[test]
    fn exit_line_offers_the_session_id_for_an_unsafe_name() {
        assert_eq!(resume_arg(Some("a $b")), SESSION);
        assert_eq!(resume_arg(Some("demo$(calc)")), SESSION);
    }

    /// An unnamed session resumes by session id.
    #[test]
    fn exit_line_of_an_unnamed_session_resumes_by_id() {
        assert_eq!(resume_arg(None), SESSION);
    }
}

#[cfg(test)]
mod run_command_tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use dal_agent::SessionRef;
    use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
    use dal_core::{
        Answer, ClientId, Command, CommandSpec, Gen, Output, PageReq, Reply, RequestId, Seq,
        SessionId, View, Workspace,
    };
    use tokio::sync::Notify;

    use super::{InflightRun, Session, run, start_run};
    use crate::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
    use crate::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions};

    /// An agent whose `submit` waits for the test, like a plugin command
    /// holding on a grant question that only the terminal can answer.
    #[derive(Clone)]
    struct Held(Arc<Notify>, SessionId);

    struct Silent;

    impl TuiSubscription for Silent {
        fn next(
            &mut self,
        ) -> impl std::future::Future<Output = Result<Option<TuiDelivery>, TuiError>> {
            std::future::ready(Ok(None))
        }
    }

    impl TuiAgent for Held {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            self.1
        }

        fn view(
            &self,
            _page: PageReq,
        ) -> impl std::future::Future<Output = Result<View, TuiError>> {
            std::future::ready(Err(TuiError::Terminal(
                "the run helper reads no view".into(),
            )))
        }

        fn subscribe(
            &self,
            _after: Option<(Gen, Seq)>,
        ) -> impl Future<Output = Result<Silent, TuiError>> {
            std::future::ready(Ok(Silent))
        }

        async fn submit(&self, _command: Command) -> Result<Reply, TuiError> {
            self.0.notified().await;
            Ok(Reply::Done(Output::Nothing))
        }

        fn answer(
            &self,
            _id: RequestId,
            _answer: Answer,
        ) -> impl Future<Output = Result<(), TuiError>> {
            std::future::ready(Ok(()))
        }

        fn ext_status(&self) -> Vec<dal_core::ExtStatus> {
            Vec::new()
        }
    }

    /// A host whose sessions open but never produce a view, recording closes.
    #[derive(Clone)]
    struct Recording {
        id: SessionId,
        closed: Arc<Mutex<Vec<SessionId>>>,
    }

    impl TuiHost for Recording {
        type Agent = Held;

        fn open(
            &self,
            _session: SessionRef,
            _client: ClientId,
        ) -> impl Future<Output = Result<Held, TuiError>> {
            std::future::ready(Ok(Held(Arc::new(Notify::new()), self.id)))
        }

        fn commands(&self) -> impl Future<Output = Result<Arc<[CommandSpec]>, TuiError>> {
            std::future::ready(Ok(Arc::from(Vec::new())))
        }

        fn close(&self, id: SessionId) -> impl Future<Output = Result<(), TuiError>> {
            self.closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(id);
            std::future::ready(Ok(()))
        }

        fn login(
            &self,
            _provider: &str,
            _method: Method,
            _io: LoginIo,
        ) -> impl Future<Output = Result<LoginOutcome, TuiError>> {
            std::future::ready(Err(TuiError::Terminal("sign-in is not under test".into())))
        }

        fn logout(
            &self,
            _provider: Option<&str>,
        ) -> impl Future<Output = Result<Vec<Box<str>>, TuiError>> {
            std::future::ready(Ok(Vec::new()))
        }

        fn stored_credentials(
            &self,
        ) -> impl Future<Output = Result<Vec<StoredCredential>, TuiError>> {
            std::future::ready(Ok(Vec::new()))
        }
    }

    #[test]
    fn a_session_whose_first_view_fails_is_closed_before_run_returns() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let workspace = Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let opts = TuiOptions {
            session: SessionRef::Ephemeral { workspace },
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
                ..EnvFacts::default()
            },
            rt: runtime.handle().clone(),
        };
        let host = Recording {
            id: SessionId::new_v7(),
            closed: Arc::default(),
        };
        let io = crate::term::fake_term_io(Vec::new());

        let outcome = run(&host, &opts, &io, || Ok(Vec::new()), |_| Ok(()));

        assert!(
            matches!(outcome, Err(TuiError::Terminal(_))),
            "the view error is the one returned: {outcome:?}"
        );
        let closed = host
            .closed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(closed, vec![host.id], "the opened session was released");
    }

    fn run_command() -> Command {
        Command::Run {
            name: "goal".into(),
            args: "".into(),
            expected: None,
        }
    }

    #[test]
    fn a_run_command_waits_off_the_loop_thread_for_its_reply() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let held = Held(Arc::new(Notify::new()), SessionId::new_v7());
        let mut session = Session {
            inflight: Some(
                start_run(runtime.handle(), &held, run_command())
                    .expect("the worker thread starts"),
            ),
            ..Session::default()
        };
        assert!(
            session.settled_run().is_none(),
            "the loop is free while the command still waits"
        );
        assert!(session.inflight.is_some());

        held.0.notify_one();
        let deadline = Instant::now() + Duration::from_secs(5);
        let settled = loop {
            if let Some(settled) = session.settled_run() {
                break settled;
            }
            assert!(Instant::now() < deadline, "the reply never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            matches!(settled, Ok(Reply::Done(Output::Nothing))),
            "{settled:?}"
        );
        assert!(session.inflight.is_none(), "one settle clears the slot");
    }

    #[test]
    fn a_worker_that_ends_without_a_reply_settles_as_an_error() {
        let (reply, settled) = std::sync::mpsc::channel();
        drop(reply);
        let mut session = Session {
            inflight: Some(InflightRun {
                settled,
                abandon: Arc::new(Notify::new()),
                worker: None,
            }),
            ..Session::default()
        };
        assert!(matches!(
            session.settled_run(),
            Some(Err(TuiError::Terminal(_)))
        ));
        assert!(session.inflight.is_none());
    }

    #[test]
    fn dropping_a_waiting_run_gives_up_the_wait_and_joins_its_worker() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let held = Held(Arc::new(Notify::new()), SessionId::new_v7());
        let inflight =
            start_run(runtime.handle(), &held, run_command()).expect("the worker thread starts");
        let (done, joined) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(inflight);
            let _ = done.send(());
        });
        assert!(
            joined.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the worker must stop without the host ever replying"
        );
    }
}

#[cfg(test)]
mod panic_hook_tests {
    use std::sync::{Arc, Mutex};

    use super::{PanicHookGuard, PanicRestorers};
    use crate::term::TermState;

    fn fresh() -> Arc<Mutex<TermState>> {
        Arc::new(Mutex::new(TermState::new()))
    }

    /// The kitty flag of every state the hook would restore, as a stand-in for
    /// "which terminal, with which modes".
    fn restored(restorers: &PanicRestorers) -> Vec<bool> {
        let mut kitty = Vec::new();
        restorers.restore_all(|saved| kitty.push(saved.kitty()));
        kitty.sort_unstable();
        kitty
    }

    #[test]
    fn the_hook_restores_each_live_state_as_it_is_when_the_panic_strikes() {
        let restorers = PanicRestorers::new();
        let (first, second) = (fresh(), fresh());
        restorers.add(&first);
        restorers.add(&second);
        first.lock().expect("state").set_kitty(true);
        assert_eq!(
            restored(&restorers),
            [false, true],
            "every pass is restored once, with the modes it holds now"
        );
    }

    #[test]
    fn forgetting_a_state_drops_exactly_that_state_and_every_reference_to_it() {
        let restorers = PanicRestorers::new();
        let (first, second) = (fresh(), fresh());
        restorers.add(&first);
        restorers.add(&second);
        second.lock().expect("state").set_kitty(true);

        restorers.remove(&first);
        assert_eq!(restored(&restorers), [true], "only the second pass is live");
        assert_eq!(
            Arc::strong_count(&first),
            1,
            "the list keeps nothing it was told to forget"
        );

        restorers.remove(&first);
        restorers.remove(&second);
        assert!(restored(&restorers).is_empty());
        assert_eq!(Arc::strong_count(&second), 1);
    }

    /// A panic that unwinds a pass drops its guard on a panicking thread, where
    /// `take_hook` and `set_hook` panic and a panic inside a destructor during
    /// unwinding aborts the process. The drop must leave the hook alone, let the
    /// original panic reach the joiner, and keep nothing of the pass.
    #[test]
    fn a_pass_unwound_by_a_panic_drops_its_guard_without_touching_the_hook() {
        let state = fresh();
        let pass = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                let _guard = PanicHookGuard::install(state);
                panic!("the pass panicked");
            })
        };
        let payload = pass.join().expect_err("the panic reaches the joiner");
        assert_eq!(
            payload.downcast_ref::<&str>().copied(),
            Some("the pass panicked")
        );
        assert_eq!(
            Arc::strong_count(&state),
            1,
            "the unwound pass left its terminal nowhere in the hook's reach"
        );
    }

    /// Passes alive together end in any order. A hook swapped in and out per pass
    /// leaves the first pass's hook installed, holding its terminal, when the
    /// first pass ends first.
    #[test]
    fn overlapping_passes_ending_in_start_order_leave_no_terminal_behind() {
        let (first, second) = (fresh(), fresh());
        let outer = PanicHookGuard::install(Arc::clone(&first));
        let inner = PanicHookGuard::install(Arc::clone(&second));
        drop(outer);
        drop(inner);
        assert_eq!(Arc::strong_count(&first), 1);
        assert_eq!(Arc::strong_count(&second), 1);
    }
}

#[cfg(test)]
mod pump_tests {
    use std::num::NonZeroU64;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use dal_core::{Gen, JobId, Seq, Update, UpdateKind};

    use super::{PUMP_DEPTH, Pump};
    use crate::TuiError;
    use crate::backend::{TuiDelivery, TuiSubscription};

    /// Hands out `total` updates at once, then fails, and counts every update
    /// the pump asked for.
    struct Burst {
        produced: Arc<AtomicUsize>,
        total: usize,
    }

    impl TuiSubscription for Burst {
        fn next(
            &mut self,
        ) -> impl std::future::Future<Output = Result<Option<TuiDelivery>, TuiError>> {
            let index = self.produced.load(Ordering::SeqCst);
            if index == self.total {
                return std::future::ready(Err(TuiError::Terminal("the burst ended".to_owned())));
            }
            self.produced.store(index + 1, Ordering::SeqCst);
            let seq = u64::try_from(index).expect("small index") + 1;
            std::future::ready(Ok(Some(TuiDelivery::Update(Arc::new(Update {
                r#gen: Gen::new(NonZeroU64::MIN),
                seq: Seq::new(NonZeroU64::new(seq).expect("nonzero")),
                kind: UpdateKind::JobSettled {
                    job: JobId::new_v7(),
                },
            })))))
        }
    }

    #[test]
    fn a_stalled_loop_bounds_the_queue_and_loses_nothing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let produced = Arc::new(AtomicUsize::new(0));
        let total = PUMP_DEPTH * 4;
        let pump = Pump::spawn(
            runtime.handle(),
            Burst {
                produced: Arc::clone(&produced),
                total,
            },
        );

        while produced.load(Ordering::SeqCst) <= PUMP_DEPTH {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            produced.load(Ordering::SeqCst),
            PUMP_DEPTH + 1,
            "a full queue plus the one delivery in hand, and no more"
        );

        for expected in 1..=total {
            let delivery = pump
                .deliveries
                .recv_timeout(Duration::from_secs(5))
                .expect("every delivery arrives")
                .expect("an update, not an error");
            let TuiDelivery::Update(update) = delivery else {
                panic!("the burst only carries updates");
            };
            assert_eq!(
                update.seq,
                Seq::new(
                    NonZeroU64::new(u64::try_from(expected).expect("small")).expect("nonzero")
                ),
                "deliveries stay in order"
            );
        }
        let failure = pump
            .deliveries
            .recv_timeout(Duration::from_secs(5))
            .expect("the failure is queued behind the updates");
        assert!(matches!(failure, Err(TuiError::Terminal(_))), "{failure:?}");
        assert!(
            pump.deliveries
                .recv_timeout(Duration::from_secs(5))
                .is_err_and(|error| error == std::sync::mpsc::RecvTimeoutError::Disconnected),
            "the pump ends after delivering the failure"
        );
    }

    #[test]
    fn dropping_a_blocked_pump_stops_its_worker() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let produced = Arc::new(AtomicUsize::new(0));
        let pump = Pump::spawn(
            runtime.handle(),
            Burst {
                produced: Arc::clone(&produced),
                total: PUMP_DEPTH * 4,
            },
        );
        while produced.load(Ordering::SeqCst) <= PUMP_DEPTH {
            std::thread::sleep(Duration::from_millis(1));
        }
        let (done, joined) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(pump);
            let _ = done.send(());
        });
        assert!(
            joined.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a worker waiting on a full queue still honours the stop flag"
        );
    }
}

#[cfg(test)]
mod loop_tests {
    use std::collections::VecDeque;
    use std::num::NonZeroU64;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use dal_agent::SessionRef;
    use dal_agent::login::{LoginIo, LoginOutcome, Method, StoredCredential};
    use dal_core::{
        Answer, ApprovalMode, AutoCompaction, ClientId, Command, CommandSpec, Gen, Mode, Output,
        Page, PageReq, Part, Reply, RequestId, Seq, SessionId, SessionInfo, SettingsView, Stats,
        ThinkingLevel, TreeOutline, TurnState, Usage, UsageView, View, Workspace,
    };

    use crate::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
    use crate::term::TermIo;
    use crate::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiError, TuiOptions, WidthMode};

    /// Probe replies from a plain terminal: no kitty keyboard, DA1 last.
    const PROBE_REPLIES: &[&[u8]] = &[
        b"\x1b[?2026;2$y",
        b"\x1b[?2027;2$y",
        b"\x1b]11;rgb:0000/0000/0000\x07",
        b"\x1b[?1;2c",
    ];

    /// What the fake host saw, in order.
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<String>>>);

    impl Log {
        fn push(&self, event: impl Into<String>) {
            self.0.lock().expect("log").push(event.into());
        }

        fn events(&self) -> Vec<String> {
            self.0.lock().expect("log").clone()
        }
    }

    /// Logs when the future holding it is dropped.
    struct LogOnDrop(Log, &'static str);

    impl Drop for LogOnDrop {
        fn drop(&mut self) {
            self.0.push(self.1);
        }
    }

    struct Silent;

    impl TuiSubscription for Silent {
        fn next(
            &mut self,
        ) -> impl std::future::Future<Output = Result<Option<TuiDelivery>, TuiError>> {
            std::future::ready(Ok(None))
        }
    }

    /// A session whose `Run` commands never reply and whose prompts are
    /// recorded, so the terminal loop runs against a host it does not own.
    #[derive(Clone)]
    struct FakeAgent {
        log: Log,
        run_entered: Arc<AtomicBool>,
    }

    fn text_of(content: &[Part]) -> String {
        content
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_ref()),
                _ => None,
            })
            .collect()
    }

    impl TuiAgent for FakeAgent {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            std::future::ready(Ok(idle_view()))
        }

        fn subscribe(
            &self,
            _after: Option<(Gen, Seq)>,
        ) -> impl Future<Output = Result<Silent, TuiError>> {
            std::future::ready(Ok(Silent))
        }

        async fn submit(&self, command: Command) -> Result<Reply, TuiError> {
            match command {
                Command::Run { .. } => {
                    let _abandoned = LogOnDrop(self.log.clone(), "run:abandoned");
                    self.log.push("run:entered");
                    self.run_entered.store(true, Ordering::SeqCst);
                    std::future::pending().await
                }
                Command::Prompt { content, .. } => {
                    self.log.push(format!("prompt:{}", text_of(&content)));
                    Ok(Reply::Done(Output::Nothing))
                }
                _ => Ok(Reply::Done(Output::Nothing)),
            }
        }

        fn answer(
            &self,
            _id: RequestId,
            _answer: Answer,
        ) -> impl Future<Output = Result<(), TuiError>> {
            std::future::ready(Ok(()))
        }

        fn ext_status(&self) -> Vec<dal_core::ExtStatus> {
            Vec::new()
        }
    }

    #[derive(Clone)]
    struct FakeHost {
        agent: FakeAgent,
    }

    impl TuiHost for FakeHost {
        type Agent = FakeAgent;

        fn open(
            &self,
            _session: SessionRef,
            _client: ClientId,
        ) -> impl Future<Output = Result<FakeAgent, TuiError>> {
            std::future::ready(Ok(self.agent.clone()))
        }

        fn commands(&self) -> impl Future<Output = Result<Arc<[CommandSpec]>, TuiError>> {
            std::future::ready(Ok(Arc::from(Vec::new())))
        }

        fn close(&self, _id: SessionId) -> impl Future<Output = Result<(), TuiError>> {
            self.agent.log.push("host:close");
            std::future::ready(Ok(()))
        }

        fn login(
            &self,
            _provider: &str,
            _method: Method,
            _io: LoginIo,
        ) -> impl Future<Output = Result<LoginOutcome, TuiError>> {
            std::future::ready(Err(TuiError::Terminal(
                "the fake host has no providers".into(),
            )))
        }

        fn logout(
            &self,
            _provider: Option<&str>,
        ) -> impl Future<Output = Result<Vec<Box<str>>, TuiError>> {
            std::future::ready(Ok(Vec::new()))
        }

        fn stored_credentials(
            &self,
        ) -> impl Future<Output = Result<Vec<StoredCredential>, TuiError>> {
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
                updated_at: dal_core::Timestamp::UNIX_EPOCH,
                created_at: Some(dal_core::Timestamp::UNIX_EPOCH),
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

    /// A `Rename` publishes only `UpdateKind::Settings`, but the exit line
    /// reads the derived `view.session.name` copy. Folding the update must
    /// keep that copy in step with the authoritative settings name;
    /// otherwise `/name demo` replies `demo` while the exit offers the id.
    #[test]
    fn settings_update_keeps_the_exit_name_in_step_with_rename() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let workspace = Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let opts = TuiOptions {
            session: SessionRef::Ephemeral { workspace },
            screen: Screen::Inline,
            theme_request: ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "true".into(),
            color: ColorMode::Never,
            binary: "dalgon",
            env: EnvFacts::default(),
            rt: runtime.handle().clone(),
        };
        let diagram_cache = crate::diagram::RenderCache::default();
        let mut surfaces = super::Surfaces {
            view: idle_view(),
            session: super::Session::default(),
            dialog: crate::dialog::DialogUi::default(),
            live: crate::live::Live::default(),
            transcript: crate::transcript::Transcript::default(),
            columns: 80,
            diagram_settings: crate::diagram::DiagramSettings::default(),
            diagram_generation: diagram_cache.generation(),
            diagram_cache,
            branch: None,
        };
        let agent = FakeAgent {
            log: Log::default(),
            run_entered: Arc::new(AtomicBool::new(false)),
        };
        let mut settings = surfaces.view.settings.clone();
        settings.name = Some("demo".into());
        let update = dal_core::Update {
            r#gen: Gen::new(NonZeroU64::MIN),
            seq: Seq::new(NonZeroU64::new(2).expect("nonzero seq")),
            kind: dal_core::UpdateKind::Settings(settings),
        };
        surfaces
            .apply_update(&opts, &agent, &update)
            .expect("settings fold");
        assert_eq!(surfaces.view.settings.name.as_deref(), Some("demo"));
        assert_eq!(
            surfaces.view.session.name.as_deref(),
            Some("demo"),
            "the exit line reads the session copy, so it must track renames"
        );
        let (exit, _, _) = surfaces.finish(&opts);
        assert_eq!(exit.name.as_deref(), Some("demo"));
        let id = exit.session.expect("persisted session");
        assert_eq!(
            dal_core::resume_token(exit.name.as_deref().expect("named"), id),
            "demo",
            "exit token must equal the `/name` reply token"
        );
    }

    /// One read's worth of input, optionally held back until a flag is set.
    struct Chunk {
        after: Option<Arc<AtomicBool>>,
        bytes: Vec<u8>,
    }

    /// A terminal whose reads hand out one scripted chunk each, in order,
    /// with no clock: a chunk is available as soon as its gate opens.
    struct ScriptedTerminal {
        chunks: Mutex<VecDeque<Chunk>>,
    }

    impl TermIo for ScriptedTerminal {
        fn enable_raw(&self) -> std::io::Result<()> {
            Ok(())
        }

        fn disable_raw(&self) {}

        fn size(&self) -> std::io::Result<(u16, u16)> {
            Ok((80, 24))
        }

        fn write(&self, _bytes: &[u8]) -> std::io::Result<()> {
            Ok(())
        }

        fn read(&self, timeout: Duration) -> std::io::Result<Vec<u8>> {
            let mut chunks = self.chunks.lock().expect("script");
            let open = chunks.front().is_some_and(|chunk| {
                chunk
                    .after
                    .as_ref()
                    .is_none_or(|gate| gate.load(Ordering::SeqCst))
            });
            if open && let Some(chunk) = chunks.pop_front() {
                return Ok(chunk.bytes);
            }
            drop(chunks);
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            Ok(Vec::new())
        }

        fn raise_tstp(&self) {}
    }

    fn chunk(bytes: &[u8]) -> Chunk {
        Chunk {
            after: None,
            bytes: bytes.to_vec(),
        }
    }

    /// The options every loop test runs under: an inline, plain-colour terminal.
    fn options(rt: tokio::runtime::Handle) -> TuiOptions {
        let workspace = Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        TuiOptions {
            session: SessionRef::Ephemeral { workspace },
            screen: Screen::Inline,
            theme_request: ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "vi".into(),
            color: ColorMode::Never,
            binary: "dalgon",
            env: EnvFacts {
                stdin_tty: true,
                term: Some("xterm-256color".to_owned()),
                width_mode: WidthMode::Narrow,
                ..EnvFacts::default()
            },
            rt,
        }
    }

    /// Runs the terminal loop against the fake host and returns what the host saw.
    fn drive(script: Vec<Chunk>, run_entered: Arc<AtomicBool>) -> Vec<String> {
        static ONE_LOOP_AT_A_TIME: Mutex<()> = Mutex::new(());
        let _serial = ONE_LOOP_AT_A_TIME
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let log = Log::default();
        let host = FakeHost {
            agent: FakeAgent {
                log: log.clone(),
                run_entered,
            },
        };
        let workspace = Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let opts = TuiOptions {
            session: SessionRef::Ephemeral { workspace },
            screen: Screen::Inline,
            theme_request: ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "vi".into(),
            color: ColorMode::Never,
            binary: "dalgon",
            env: EnvFacts {
                stdin_tty: true,
                term: Some("xterm-256color".to_owned()),
                width_mode: WidthMode::Narrow,
                ..EnvFacts::default()
            },
            rt: runtime.handle().clone(),
        };
        let terminal = ScriptedTerminal {
            chunks: Mutex::new(script.into()),
        };
        super::run(&host, &opts, &terminal, || Ok(Vec::new()), |_| Ok(()))
            .expect("the loop exits cleanly");
        log.events()
    }

    #[test]
    fn quitting_with_a_run_in_flight_abandons_it_before_the_host_closes() {
        let run_entered = Arc::new(AtomicBool::new(false));
        let mut script: Vec<Chunk> = PROBE_REPLIES.iter().map(|reply| chunk(reply)).collect();
        script.push(chunk(b"/goal\r"));
        script.push(Chunk {
            after: Some(Arc::clone(&run_entered)),
            bytes: b"\x04".to_vec(),
        });
        assert_eq!(
            drive(script, run_entered),
            ["run:entered", "run:abandoned", "host:close"],
            "the slow command is cancelled and its worker joined while the host is still open"
        );
    }

    #[test]
    fn input_typed_during_the_probe_is_replayed_and_no_reply_reaches_the_composer() {
        let script = vec![
            chunk(b"hel\x1b[?2026;2"),
            chunk(b"$y\x1b[?2027;2$ylo\r"),
            chunk(b"\x1b]11;rgb:0000/00"),
            chunk(b"00/0000\x07\x1b[?1;2c"),
            chunk(b"world\r"),
            chunk(b"\x04"),
        ];
        assert_eq!(
            drive(script, Arc::new(AtomicBool::new(false))),
            ["prompt:hello", "prompt:world", "host:close"],
            "the first prompt survives the probe and the second carries no reply bytes"
        );
    }

    /// A session whose journal the test appends to: `view` answers with every
    /// entry appended so far, as the host does after the same updates.
    #[derive(Clone, Default)]
    struct Ledger(Arc<Mutex<Vec<dal_core::EntryView>>>);

    impl TuiAgent for Ledger {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            let mut view = idle_view();
            view.entries.items = self.0.lock().expect("ledger").clone();
            std::future::ready(Ok(view))
        }

        fn subscribe(
            &self,
            _after: Option<(Gen, Seq)>,
        ) -> impl Future<Output = Result<Silent, TuiError>> {
            std::future::ready(Ok(Silent))
        }

        fn submit(&self, _command: Command) -> impl Future<Output = Result<Reply, TuiError>> {
            std::future::ready(Ok(Reply::Done(Output::Nothing)))
        }

        fn answer(
            &self,
            _id: RequestId,
            _answer: Answer,
        ) -> impl Future<Output = Result<(), TuiError>> {
            std::future::ready(Ok(()))
        }

        fn ext_status(&self) -> Vec<dal_core::ExtStatus> {
            Vec::new()
        }
    }

    /// The loop's surfaces over a ledger agent. Updates enter through the same
    /// `absorb_update` the drain takes; a result lands in the ledger first, so
    /// the turn-end snapshot holds exactly the entries the updates announced.
    struct Rig {
        surfaces: super::Surfaces,
        agent: Ledger,
        opts: TuiOptions,
        seq: u64,
        entries: u64,
        _runtime: tokio::runtime::Runtime,
    }

    impl Rig {
        fn new() -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let opts = options(runtime.handle().clone());
            let diagram_cache = crate::diagram::RenderCache::default();
            let surfaces = super::Surfaces {
                view: idle_view(),
                session: super::Session::default(),
                dialog: crate::dialog::DialogUi::default(),
                live: crate::live::Live::default(),
                transcript: crate::transcript::Transcript::default(),
                columns: 80,
                diagram_settings: crate::diagram::DiagramSettings::default(),
                diagram_generation: diagram_cache.generation(),
                diagram_cache,
                branch: None,
            };
            Self {
                surfaces,
                agent: Ledger::default(),
                opts,
                seq: 0,
                entries: 0,
                _runtime: runtime,
            }
        }

        fn feed(&mut self, kind: dal_core::UpdateKind) {
            self.seq += 1;
            let update = dal_core::Update {
                r#gen: Gen::new(NonZeroU64::MIN),
                seq: Seq::new(NonZeroU64::new(self.seq).expect("the counter starts at one")),
                kind,
            };
            self.surfaces
                .absorb_update(&self.opts, &self.agent, &update)
                .expect("the update folds");
        }

        fn start(&mut self, call: &str, tool: &str) {
            self.feed(dal_core::UpdateKind::ToolStarted {
                call: dal_core::CallId::new(call),
                tool: tool.into(),
                args: dal_core::RawJson::null(),
            });
        }

        fn settle(&mut self, call: &str, elapsed_ms: Option<u64>) {
            self.feed(dal_core::UpdateKind::ToolSettled {
                call: dal_core::CallId::new(call),
                outcome: dal_core::ToolOutcomeView {
                    is_error: false,
                    text: "settled".into(),
                    images: Vec::new(),
                    elapsed_ms,
                },
            });
        }

        /// Journals the call's result entry, summarised as `out-{call}`, and
        /// announces it the way the fold does: as a tree delta.
        fn result(&mut self, call: &str, tool: &str, elapsed_ms: Option<u64>) {
            self.entries += 1;
            let id = dal_core::EntryId::new(
                NonZeroU64::new(self.entries).expect("the counter starts at one"),
            );
            let entry = dal_core::EntryView {
                id,
                parent: None,
                kind: dal_core::EntryKind::ToolResult {
                    call: dal_core::CallId::new(call),
                    name: tool.into(),
                    error: false,
                    parts: vec![dal_core::JournalPart::Text {
                        text: format!("out-{call}").into(),
                    }],
                    changes: Vec::new(),
                    elapsed_ms,
                },
            };
            self.agent.0.lock().expect("ledger").push(entry.clone());
            self.feed(dal_core::UpdateKind::Tree(dal_core::TreeDelta {
                added: vec![entry],
                leaf: Some(id),
            }));
        }

        fn end(&mut self, stop: dal_core::Stop) {
            self.feed(dal_core::UpdateKind::TurnEnded {
                turn: dal_core::TurnId::new(NonZeroU64::MIN),
                stop,
            });
        }

        fn tracked(&self) -> (usize, usize) {
            self.surfaces.live.tracked_tools()
        }

        fn rows(&self) -> Vec<String> {
            self.surfaces.transcript.rows().to_vec()
        }

        fn duration(&self, call: &str) -> Option<Duration> {
            self.surfaces.transcript.tool_duration(call)
        }

        /// The committed rows that name this call's result.
        fn rows_of(&self, call: &str) -> Vec<String> {
            let summary = format!("out-{call}");
            self.rows()
                .into_iter()
                .filter(|row| row.contains(&summary))
                .collect()
        }
    }

    #[test]
    fn a_result_before_its_settle_hands_off_timing_and_releases_the_card() {
        let mut rig = Rig::new();
        rig.start("call-1", "exec");
        assert_eq!(rig.tracked(), (1, 1));
        assert_eq!(rig.surfaces.live.running_tool_rows(), ["working  exec"]);

        // The result entry lands first, carrying the engine's own figure.
        rig.result("call-1", "exec", Some(40));
        assert_eq!(rig.tracked(), (0, 0), "the commit releases the live card");
        assert_eq!(rig.surfaces.live.running_tool_rows().len(), 0);
        let measured = rig
            .duration("call-1")
            .expect("the elapsed time moved to the transcript with the release");
        let committed = rig.rows();
        let card = rig.rows_of("call-1");
        assert_eq!(card.len(), 1, "{committed:?}");
        assert!(card[0].contains(&crate::copy::dur(measured)), "{card:?}");

        // The settle that trails it finds no card: nothing revives, and the
        // duration and rows the transcript holds do not move.
        rig.settle("call-1", Some(3_600_000));
        assert_eq!(rig.tracked(), (0, 0));
        assert_eq!(rig.surfaces.live.activity(), None);
        assert_eq!(rig.duration("call-1"), Some(measured));
        assert_eq!(rig.rows(), committed);
    }

    #[test]
    fn a_settle_before_its_result_commits_the_settled_timing_then_releases() {
        let mut rig = Rig::new();
        rig.start("call-1", "exec");
        rig.settle("call-1", Some(1500));
        assert_eq!(rig.tracked(), (1, 1), "the card waits for its result entry");
        assert_eq!(rig.surfaces.live.running_tool_rows().len(), 0);

        // The entry carries no figure, so the transcript's own record is the
        // only source of the settled card's duration.
        rig.result("call-1", "exec", None);
        assert_eq!(rig.tracked(), (0, 0));
        let settled = Duration::from_millis(1500);
        assert_eq!(rig.duration("call-1"), Some(settled));
        let card = rig.rows_of("call-1");
        assert_eq!(card.len(), 1);
        assert!(card[0].contains(&crate::copy::dur(settled)), "{card:?}");
    }

    #[test]
    fn a_result_releases_only_its_own_call_while_siblings_keep_working() {
        let mut rig = Rig::new();
        rig.start("read-1", "read");
        rig.start("exec-1", "exec");
        rig.start("grep-1", "grep");

        rig.result("exec-1", "exec", Some(9));
        assert_eq!(rig.tracked(), (2, 2));
        assert_eq!(
            rig.surfaces.live.running_tool_rows(),
            ["working  read", "working  grep"]
        );
        assert_eq!(
            rig.surfaces.live.activity(),
            Some(crate::copy::ids::STATE_WORKING)
        );
        assert!(rig.duration("exec-1").is_some());
        assert_eq!(
            rig.duration("read-1"),
            None,
            "a working call has no history yet"
        );

        rig.settle("exec-1", Some(9));
        assert_eq!(
            rig.tracked(),
            (2, 2),
            "a trailing settle touches no sibling"
        );
        rig.result("read-1", "read", Some(4));
        rig.result("grep-1", "grep", Some(5));
        assert_eq!(rig.tracked(), (0, 0));
        assert_eq!(rig.surfaces.live.activity(), None);
    }

    #[test]
    fn a_cancelled_turn_releases_unanswered_calls_and_keeps_every_completed_row() {
        let mut rig = Rig::new();
        rig.start("done", "read");
        rig.start("lost", "exec");
        rig.start("also-lost", "grep");
        rig.result("done", "read", Some(3));
        let committed = rig.rows();
        assert_eq!(rig.tracked(), (2, 2));

        rig.end(dal_core::Stop::Cancelled);
        assert_eq!(rig.tracked(), (0, 0), "no card outlives its turn");
        assert_eq!(rig.surfaces.live.running_tool_rows().len(), 0);
        assert_eq!(rig.surfaces.live.activity(), None);
        assert_eq!(rig.rows(), committed, "the turn end repaints nothing");
        assert!(rig.duration("done").is_some());
        assert_eq!(rig.duration("lost"), None);
        assert_eq!(rig.duration("also-lost"), None);
        assert_eq!(rig.rows_of("done").len(), 1);

        // A trailing update for a released leftover stays inert.
        rig.settle("lost", Some(1));
        assert_eq!(rig.tracked(), (0, 0));
    }

    #[test]
    fn many_turns_leave_the_live_region_empty_and_the_transcript_whole() {
        let mut rig = Rig::new();
        let mut completed = Vec::new();
        let mut lost = Vec::new();
        let mut previous: Vec<String> = Vec::new();
        for turn in 0..120_u64 {
            let first = format!("turn{turn}-first");
            let second = format!("turn{turn}-second");
            let cancelled = turn.is_multiple_of(4);
            rig.start(&first, "read");
            rig.start(&second, "exec");
            if cancelled {
                let stray = format!("turn{turn}-lost");
                rig.start(&stray, "grep");
                lost.push(stray);
            }
            if turn.is_multiple_of(2) {
                rig.result(&first, "read", None);
                rig.result(&second, "exec", Some(2));
                rig.settle(&first, Some(11));
                rig.settle(&second, Some(12));
            } else {
                rig.settle(&first, Some(11));
                rig.settle(&second, Some(12));
                rig.result(&first, "read", None);
                rig.result(&second, "exec", Some(2));
            }
            rig.end(if cancelled {
                dal_core::Stop::Cancelled
            } else {
                dal_core::Stop::EndTurn
            });
            assert_eq!(rig.tracked(), (0, 0), "turn {turn} left live tool state");
            assert_eq!(rig.surfaces.live.activity(), None);
            completed.extend([first, second]);

            // Settled rows only ever append: nothing earlier is repainted.
            let rows = rig.rows();
            assert!(rows.starts_with(&previous), "turn {turn} rewrote history");
            previous = rows;
        }
        for call in &completed {
            let duration = rig
                .duration(call)
                .expect("a completed call keeps its timing");
            let card = rig.rows_of(call);
            assert_eq!(card.len(), 1, "{call}: {card:?}");
            assert!(
                card[0].contains(&crate::copy::dur(duration)),
                "{call}: {card:?}"
            );
        }
        for call in &lost {
            assert_eq!(rig.duration(call), None, "{call} never had a result");
            assert_eq!(rig.rows_of(call).len(), 0);
        }
    }

    #[derive(Clone)]
    struct FailingAnswerAgent;

    impl TuiAgent for FailingAnswerAgent {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            std::future::ready(Ok(idle_view()))
        }

        fn subscribe(
            &self,
            _after: Option<(Gen, Seq)>,
        ) -> impl Future<Output = Result<Silent, TuiError>> {
            std::future::ready(Ok(Silent))
        }

        fn submit(&self, _command: Command) -> impl Future<Output = Result<Reply, TuiError>> {
            std::future::ready(Ok(Reply::Done(Output::Nothing)))
        }

        fn answer(
            &self,
            id: RequestId,
            _answer: Answer,
        ) -> impl Future<Output = Result<(), TuiError>> {
            std::future::ready(Err(TuiError::Agent(
                dal_agent::AgentError::AlreadyResolved {
                    id,
                    by: ClientId::new("rpc-1"),
                },
            )))
        }

        fn ext_status(&self) -> Vec<dal_core::ExtStatus> {
            Vec::new()
        }
    }

    /// A session whose `view` reports one fixed committed scope, or fails.
    /// The surfaces under test keep a view that says "no scope", so a picker
    /// checked from this agent's answer proves the seed is a fresh read.
    #[derive(Clone)]
    struct ScopeAgent {
        scope: Option<Vec<Box<str>>>,
        view_fails: bool,
    }

    impl TuiAgent for ScopeAgent {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            std::future::ready(if self.view_fails {
                Err(TuiError::Terminal("the view is unavailable".to_owned()))
            } else {
                let mut view = idle_view();
                view.settings.scoped_models = self.scope.clone();
                Ok(view)
            })
        }

        fn subscribe(
            &self,
            _after: Option<(Gen, Seq)>,
        ) -> impl Future<Output = Result<Silent, TuiError>> {
            std::future::ready(Ok(Silent))
        }

        fn submit(&self, _command: Command) -> impl Future<Output = Result<Reply, TuiError>> {
            std::future::ready(Ok(Reply::Done(Output::Nothing)))
        }

        fn answer(
            &self,
            _id: RequestId,
            _answer: Answer,
        ) -> impl Future<Output = Result<(), TuiError>> {
            std::future::ready(Ok(()))
        }

        fn ext_status(&self) -> Vec<dal_core::ExtStatus> {
            Vec::new()
        }
    }

    /// Two scope-eligible catalog rows, in display order.
    fn scope_catalog() -> Vec<crate::picker::ModelOption> {
        ["openai/gpt-6", "anthropic/opus-4"]
            .into_iter()
            .map(|id| crate::picker::ModelOption {
                label: id.to_owned(),
                command: Command::SetModel {
                    model: dal_core::ModelRoute::Synthetic { id: id.into() },
                    save: dal_core::command::Save::SessionAndDefault,
                },
                scoped_id: Some(id.into()),
            })
            .collect()
    }

    /// The scoped-models picker checks the committed scope the agent reports
    /// right now. The terminal keeps no scope copy: its own view stays at
    /// "no scope" throughout, an unmatched committed id survives submit, and
    /// `None` and an explicit `Some([])` both open with nothing checked.
    #[test]
    fn scoped_picker_seeds_from_a_fresh_agent_view() {
        let mut rig = Rig::new();
        assert_eq!(rig.surfaces.view.settings.scoped_models, None);

        let agent = ScopeAgent {
            scope: Some(vec!["anthropic/opus-4".into(), "anthropic/vanished".into()]),
            view_fails: false,
        };
        let mut models = || Ok(scope_catalog());
        let mut picker = rig
            .surfaces
            .scoped_models_choice(&rig.opts, &agent, "", &mut models)
            .expect("a readable scope opens the picker");
        assert!(!picker.is_checked(0), "openai/gpt-6 is outside the scope");
        assert!(picker.is_checked(1), "anthropic/opus-4 is inside the scope");
        assert!(matches!(
            picker.activate_selected(),
            Some(crate::picker::PickerAction::Command(Command::SetScopedModels(ids)))
                if ids
                    .iter()
                    .map(|id| id.as_ref())
                    .eq(["anthropic/opus-4", "anthropic/vanished"])
        ));
        assert_eq!(
            rig.surfaces.view.settings.scoped_models, None,
            "the terminal never stores a scope of its own"
        );

        for scope in [None, Some(Vec::new())] {
            let agent = ScopeAgent {
                scope,
                view_fails: false,
            };
            let picker = rig
                .surfaces
                .scoped_models_choice(&rig.opts, &agent, "", &mut models)
                .expect("an empty scope still opens the picker");
            assert!(!picker.is_checked(0), "no scope checks nothing");
            assert!(!picker.is_checked(1), "no scope checks nothing");
        }
    }

    /// `/scoped-models` reaches the same fresh read through the reply fold:
    /// the chooser reply opens a picker already checked from the agent view.
    #[test]
    fn scoped_chooser_reply_opens_the_picker_on_the_agent_scope() {
        let mut rig = Rig::new();
        let agent = ScopeAgent {
            scope: Some(vec!["openai/gpt-6".into()]),
            view_fails: false,
        };
        let mut models = || Ok(scope_catalog());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &agent,
                &mut models,
                Ok(Reply::Choose {
                    chooser: dal_core::Chooser::ScopedModels,
                    filter: "".into(),
                }),
            )
            .expect("the chooser reply folds");
        let picker = rig
            .surfaces
            .session
            .picker
            .as_ref()
            .expect("the scoped picker opens");
        assert!(picker.is_checked(0), "the committed id arrives checked");
        assert!(!picker.is_checked(1));
    }

    /// A failed view read never degrades to an empty scope: submitting that
    /// guess would clear the real restriction. The failure shows as an error
    /// notice and no picker opens.
    #[test]
    fn scoped_picker_refuses_to_guess_when_the_view_read_fails() {
        let mut rig = Rig::new();
        let agent = ScopeAgent {
            scope: Some(vec!["openai/gpt-6".into()]),
            view_fails: true,
        };
        let mut models = || Ok(scope_catalog());
        let picker = rig
            .surfaces
            .scoped_models_choice(&rig.opts, &agent, "", &mut models);
        assert!(picker.is_none(), "an unreadable scope opens no picker");
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].role, crate::theme::Role::Error);
        assert!(
            notices[0]
                .text
                .starts_with("note: cannot read the current model scope"),
            "{notices:?}"
        );
    }

    /// `/new` cannot open a session in place: the refusal is an error notice
    /// with the `note:` prefix, never a plain transcript row.
    #[test]
    fn new_session_reply_is_an_error_notice() {
        let mut rig = Rig::new();
        let mut models = || Ok(Vec::new());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &rig.agent,
                &mut models,
                Ok(Reply::Front(dal_core::FrontAction::NewSession)),
            )
            .expect("the /new refusal folds into a notice");
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].role, crate::theme::Role::Error);
        assert!(notices[0].text.starts_with("note: /new"), "{notices:?}");
        assert!(rig.rows().is_empty(), "the refusal is not a transcript row");
    }

    /// Exercises the production `Surfaces` methods that emit error notices —
    /// `flush_answers`, `fold_submit_reply`, `begin_login`, and
    /// `apply_diagram_updates` — and asserts `Role::Error` plus the `note:`
    /// prefix on each, while informational diagram toggles stay `Role::Dim`.
    #[test]
    fn runtime_error_paths_emit_error_role_notices_with_the_note_prefix() {
        use crate::theme::Role;

        let mut rig = Rig::new();
        let host = FakeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
        };
        let mut models = || Ok(Vec::new());
        // The contract is the role and the word prefix; the rest of each
        // text is the failing error's own message, which this crate neither
        // owns nor pins.
        let is_error_note = |row: &crate::live::NoticeRow| {
            row.role == Role::Error && row.text.starts_with("note: ")
        };

        let lost_id = RequestId::new_v7();
        rig.surfaces
            .session
            .pending_answers
            .push((lost_id, Answer::Approve));
        rig.surfaces.flush_answers(&rig.opts, &FailingAnswerAgent);
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(is_error_note(&notices[0]), "{notices:?}");

        let closed = dal_agent::AgentError::SessionClosed {
            id: SessionId::new_v7(),
        };
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &rig.agent,
                &mut models,
                Err(TuiError::Agent(closed)),
            )
            .expect("submit error folds into a notice");
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(is_error_note(&notices[1]), "{notices:?}");

        rig.surfaces.begin_login(&rig.opts, &host, "nonexistent");
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(is_error_note(&notices[2]), "{notices:?}");
        assert!(
            notices[2].text.contains("nonexistent"),
            "the notice names the provider it refused: {notices:?}"
        );

        let mut painter = crate::screen::driver::Painter::default();
        let probe = crate::term::Probe::default();
        let mut fail_save =
            |_enabled: bool| Err(TuiError::Terminal("disk is read-only".to_owned()));
        rig.surfaces.session.pending_diagram_settings = vec![
            (true, dal_core::command::Save::SessionOnly),
            (true, dal_core::command::Save::SessionAndDefault),
        ];
        rig.surfaces
            .apply_diagram_updates(&rig.opts, &mut painter, &probe, &mut fail_save);
        let tail = rig.surfaces.live.notices();
        let last = tail.last().expect("failed save pushed a notice");
        assert!(is_error_note(last), "{tail:?}");
        assert!(
            last.text.contains("disk is read-only"),
            "the notice carries the cause of the failed save: {tail:?}"
        );
        // The row cap keeps the newest three: the refused login, the
        // session-only toggle, and the failed save. Only the toggle is
        // informational.
        let roles: Vec<Role> = tail.iter().map(|row| row.role).collect();
        assert_eq!(
            roles,
            [Role::Error, Role::Dim, Role::Error],
            "the session-only diagram toggle stays informational: {tail:?}"
        );
    }

    /// A refused OSC 52 clipboard write is a failure the user asked for: the
    /// `COPY_FAILED` row carries `Role::Error`; a successful copy stays `Dim`.
    #[test]
    fn a_refused_clipboard_write_is_an_error_notice() {
        use std::sync::atomic::AtomicUsize;

        struct Flaky {
            fail_after: usize,
            writes: AtomicUsize,
        }

        impl TermIo for Flaky {
            fn enable_raw(&self) -> std::io::Result<()> {
                Ok(())
            }
            fn disable_raw(&self) {}
            fn size(&self) -> std::io::Result<(u16, u16)> {
                Ok((80, 24))
            }
            fn write(&self, _bytes: &[u8]) -> std::io::Result<()> {
                let index = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
                if index > self.fail_after {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "the terminal refused the selection",
                    ));
                }
                Ok(())
            }
            fn read(&self, _timeout: Duration) -> std::io::Result<Vec<u8>> {
                Ok(Vec::new())
            }
            fn raise_tstp(&self) {}
        }

        let mut rig = Rig::new();
        rig.surfaces.session.pending_copies = vec!["first".into(), "second".into()];
        let io = Flaky {
            fail_after: 1,
            writes: AtomicUsize::new(0),
        };
        rig.surfaces.write_copies(&io);

        let roles: Vec<crate::theme::Role> = rig
            .surfaces
            .live
            .notices()
            .iter()
            .map(|row| row.role)
            .collect();
        assert_eq!(
            roles,
            [crate::theme::Role::Dim, crate::theme::Role::Error],
            "a successful copy stays dim; a refused clipboard write is an error: {roles:?}"
        );
        assert_eq!(
            rig.surfaces.live.notices()[1].text,
            crate::copy::ids::COPY_FAILED
        );
        assert_eq!(io.writes.load(Ordering::SeqCst), 2);
    }
}
