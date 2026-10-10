use crate::theme::ResolvedTheme;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{Method, SecretString};
use dal_core::{
    Answer, CancelScope, Chooser, ClientId, Command, Expect, FrontAction, Output, PageReq, Reply,
    RequestId, Stop, TurnId, TurnState, UpdateKind,
};

use crate::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use crate::composer::Composer;
use crate::dialog::DialogUi;
use crate::keys::{Action, InputEvent, KeyDecoder, Owner, resolve_in};
use crate::live::Live;
use crate::picker::{ModelOption, PickerAction, PickerUi};
use crate::render::{FrameInput, entry_rows_timed};
use crate::screen::driver::Painter;
use crate::signin::{self, KeyAction, SignIn};
use crate::term::{ReplyParser, TermIo, TermState, restore, startup_probe};
use crate::transcript::Transcript;
use crate::{TuiError, TuiExit, TuiOptions};

/// Draft-quit confirmation owned by the composer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitDialog {
    /// The user typed `/quit` with a non-empty draft.
    DiscardDraft,
}

const RESOLUTION_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Local composer and exit state; transport to `Agent::submit` lands with `Host::open`.
#[derive(Debug, Default)]
struct Session {
    composer: Composer,
    dialog: Option<ExitDialog>,
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
    command_seq: u64,
    commands: std::sync::Arc<[dal_core::CommandSpec]>,
    popup: Vec<String>,
    candidates: Vec<crate::popup::Candidate>,
    picker: Option<PickerUi>,
    signin: Option<SignIn>,
    front: Vec<FrontRequest>,
    cancelling: Option<TurnId>,
    cancel_settled: bool,
    cancel_deadline: Option<std::time::Instant>,
    /// The one `Run` command still executing off this thread.
    inflight: Option<InflightRun>,
}

/// A terminal-only request that needs the host; the loop runs it after the
/// input pass.
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
                    format!("/{} · {}", item.name, crate::width::escape(&item.summary))
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

    fn answer_dialog(&mut self, key: char) {
        match key {
            'y' | 'Y' => {
                self.quit = true;
                self.dialog = None;
                self.composer.clear();
            }
            'n' | 'N' => self.dialog = None,
            _ => {}
        }
    }

    fn interrupt(&mut self) {
        if let Some(turn) = self.active_turn {
            self.pending_commands.push(Command::Cancel {
                scope: CancelScope::Turn(turn),
            });
        }
    }

    /// Ctrl+C: stop a running sign-in, close a finished overlay or picker,
    /// else interrupt the turn.
    fn interrupt_or_dismiss(&mut self) {
        if let Some(signin) = self.signin.as_mut() {
            if signin.is_running() {
                signin.cancel();
            } else {
                self.signin = None;
            }
        } else if self.picker.take().is_none() {
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
                    self.dialog = Some(ExitDialog::DiscardDraft);
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
        let view = opts.rt.block_on(agent.view(snapshot_page()?))?;
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
                .block_on(agent.view(snapshot_page().ok()?))
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
        crate::copy::render(
            crate::copy::ids::EXIT_SAVED,
            &[
                ("bin", opts.binary),
                ("name", exit.name.as_deref().unwrap_or(&session)),
                ("id", &session),
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
fn snapshot_page() -> Result<PageReq, TuiError> {
    let Some(limit) = NonZeroU32::new(PageReq::DEFAULT_LIMIT) else {
        return Err(TuiError::Terminal("dal-tui: page limit is zero".to_owned()));
    };
    PageReq::new(limit, None).map_err(|error| TuiError::Terminal(error.to_string()))
}

type PanicHook = Box<dyn for<'a> Fn(&std::panic::PanicHookInfo<'a>) + Send + Sync + 'static>;

struct PanicHookGuard {
    previous: Arc<Mutex<Option<PanicHook>>>,
}

impl PanicHookGuard {
    fn install(state: Arc<Mutex<TermState>>) -> Self {
        let previous = Arc::new(Mutex::new(Some(std::panic::take_hook())));
        let owned = Arc::clone(&previous);
        std::panic::set_hook(Box::new(move |info| {
            let saved = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            restore(&saved);
            if let Some(previous) = owned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                previous(info);
            }
        }));
        Self { previous }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let _ = std::panic::take_hook();
        if let Some(previous) = self
            .previous
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            std::panic::set_hook(previous);
        }
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
    let diagram_cache = crate::diagram::RenderCache::with_path(opts.env.path.clone());
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

#[expect(
    clippy::too_many_arguments,
    reason = "the loop's inputs are the run's own locals; grouping adds a shell"
)]
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
        for event in events {
            apply_event(
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                event,
                probe.kitty_keyboard,
                opts.screen,
            );
        }
        surfaces.flush_answers(opts, agent);
        surfaces.submit_commands(opts, agent, &mut model_source)?;
        surfaces.run_front(opts, &host);
        surfaces.poll_signin(opts, io, &mut model_source);
        surfaces.write_copies(io);
        surfaces.apply_diagram_updates(opts, &mut painter, &probe, &mut save_diagrams);
        surfaces.drain(opts, agent, pump)?;
        surfaces.poll_resolution(opts, agent, &mut resolution_poll)?;
        surfaces.reseed_diagrams(opts);
        surfaces.dialog.tick(Instant::now());
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
/// never hides a failure. `Drop` stops the worker within one poll tick and
/// joins it, so neither shutdown nor resync leaks a thread.
struct Pump<S: TuiSubscription> {
    deliveries: std::sync::mpsc::Receiver<PumpItem>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
        let worker = std::thread::Builder::new()
            .name("dal-tui-delivery-pump".to_owned())
            .spawn({
                let runtime = runtime.clone();
                let stop_flag = std::sync::Arc::clone(&stop);
                move || {
                    while !stop_flag.load(Ordering::SeqCst) {
                        let tick = runtime.block_on(async {
                            tokio::time::timeout(Duration::from_millis(50), subscription.next())
                                .await
                        });
                        match tick {
                            // Idle tick: poll again until asked to stop.
                            Err(_) => {}
                            // Closed or lagged latches `None`; later calls
                            // return at once, so exit instead of spinning.
                            Ok(Ok(None)) => break,
                            Ok(Ok(Some(delivery))) => {
                                if !deliver(&sender, &stop_flag, Ok(delivery)) {
                                    break;
                                }
                            }
                            Ok(Err(error)) => {
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
                exit_draft: self.session.dialog.is_some(),
                branch: self.branch.as_deref(),
                popup: &self.session.popup,
                live: &self.live,
                dialog: &self.dialog,
                picker: self.session.picker.as_ref(),
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
                self.live.notice(format!("note: {error}"));
            }
        }
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
                    // The result entry lands before its settle update, so the
                    // card takes the time elapsed since the call started.
                    if let dal_core::EntryKind::ToolResult { call, .. } = &entry.kind
                        && let Some(duration) = self.live.tool_elapsed(call.as_str())
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
            UpdateKind::RequestResolved { id, by, .. } => {
                if let Some(title) = self.dialog.resolved(&id.to_string()) {
                    self.live.notice(crate::copy::render(
                        crate::copy::ids::REQUEST_RESOLVED_BY,
                        &[
                            ("client", by.as_str()),
                            ("title", &crate::width::escape(&title)),
                        ],
                        1,
                    ));
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
                let fresh = opts.rt.block_on(agent.view(snapshot_page()?))?;
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
            UpdateKind::Settings(settings) => self.view.settings = settings.clone(),
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
                TuiDelivery::Update(update) => {
                    self.live.apply_update(&update);

                    self.view.r#gen = update.r#gen;
                    self.view.seq = update.seq;
                    self.apply_update(opts, agent, &update)?;
                }
                TuiDelivery::Resync(snapshot) => {
                    let remote_repaint = snapshot.is_some();
                    let fresh = match snapshot {
                        Some(snapshot_view) => *snapshot_view,
                        None => opts.rt.block_on(agent.view(snapshot_page()?))?,
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
                    Err(error) => self
                        .live
                        .notice(format!("note: cannot start the command: {error}")),
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
                self.live.notice(error.to_string());
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
                            self.live
                                .notice(crate::copy::ids::MODEL_PICKER_FAIL.to_owned());
                            None
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
                    _ => {
                        self.live
                            .notice(crate::copy::ids::PICKER_UNSUPPORTED.to_owned());
                        None
                    }
                };
                self.session.picker = picker;
                Vec::new()
            }
            other => self.session.accept_reply(other),
        };
        if queued {
            self.view = opts.rt.block_on(agent.view(snapshot_page()?))?;
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

    /// Runs the host-backed requests the input pass queued.
    fn run_front<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H) {
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
                self.live.notice(format!(
                    "Unknown provider \"{}\".",
                    crate::width::escape(provider)
                ));
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
                self.live
                    .notice(crate::copy::ids::MODEL_PICKER_FAIL.to_owned());
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
            let notice = match io.write(format!("\x1b]52;c;{payload}\x07").as_bytes()) {
                Ok(()) => {
                    let count = text.chars().count();
                    crate::copy::render(
                        crate::copy::ids::COPY_DONE,
                        &[("n", &count.to_string())],
                        u64::try_from(count).unwrap_or(u64::MAX),
                    )
                }
                Err(_) => crate::copy::ids::COPY_FAILED.to_owned(),
            };
            self.live.notice(notice);
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
                    Err(error) => self.live.notice(error.to_string()),
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
    fn poll_resolution<A: TuiAgent>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        resolution_poll: &mut Instant,
    ) -> Result<(), TuiError> {
        if self.dialog.awaiting_resolution() {
            let now = Instant::now();
            if now >= *resolution_poll {
                *resolution_poll = now + RESOLUTION_POLL_INTERVAL;
                let view_request = snapshot_page()?;
                let refreshed = opts.rt.block_on(async {
                    tokio::time::timeout(RESOLUTION_POLL_INTERVAL, agent.view(view_request)).await
                });
                if let Ok(Ok(fresh)) = refreshed {
                    self.dialog.resync(fresh.open.clone());
                    self.session.active_turn = match fresh.turn {
                        TurnState::Running { turn } | TurnState::Settling { turn } => Some(turn),
                        _ => None,
                    };
                }
            }
        }
        Ok(())
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
    let Some(limit) = NonZeroU32::new(PageReq::MAX_LIMIT) else {
        return Err(TuiError::Terminal("dal-tui: page limit is zero".to_owned()));
    };
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
            apply_type_ahead(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &parser.finish(),
                opts.screen,
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
            apply_type_ahead(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &keys,
                opts.screen,
            );
            // A key event requests an immediate frame that bypasses the batch.
            surfaces.paint(io, state, painter, opts, palette)?;
        }
        if probe.da1 {
            let held = parser.finish();
            apply_type_ahead(
                decoder,
                &mut surfaces.session,
                &mut surfaces.dialog,
                &surfaces.transcript,
                &held,
                opts.screen,
            );
            break;
        }
    }
    Ok(probe)
}

/// Decodes terminal bytes into key events and applies them to the session.
/// Probe-window bytes predate the kitty switch, so they decode as legacy.
fn apply_type_ahead(
    decoder: &mut KeyDecoder,
    session: &mut Session,
    dialog: &mut DialogUi,
    transcript: &Transcript,
    bytes: &[u8],
    screen: crate::Screen,
) {
    if bytes.is_empty() {
        return;
    }
    for event in decoder.feed(bytes, Instant::now()) {
        apply_event(session, dialog, transcript, event, false, screen);
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
        _ => return false,
    }
    true
}

fn apply_event(
    session: &mut Session,
    dialog: &mut DialogUi,
    transcript: &Transcript,
    event: InputEvent,
    kitty: bool,
    screen: crate::Screen,
) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let InputEvent::Key(key) = event else {
        if let InputEvent::Paste(bytes) = event {
            if dialog.is_open() {
                dialog.paste(&bytes);
            } else if let Some(signin) = session.signin.as_mut() {
                signin.paste(&String::from_utf8_lossy(&bytes));
            } else if !session.overlay && session.dialog.is_none() && session.picker.is_none() {
                session.composer.insert_paste(&bytes);
                session.update_popup();
            }
        }
        return;
    };
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        session.interrupt_or_dismiss();
        return;
    }
    if let Some(ExitDialog::DiscardDraft) = session.dialog {
        match key.code {
            KeyCode::Char(choice) if key.modifiers == KeyModifiers::NONE => {
                session.answer_dialog(choice);
            }
            KeyCode::Esc => session.dialog = None,
            _ => {}
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
    if session.overlay {
        apply_overlay_key(session, transcript, key, kitty);
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
    match action {
        Some(Action::QuitEmptyComposer) if session.composer.is_empty() => session.quit = true,
        // Ctrl+D with a draft deletes the character under the caret.
        Some(Action::QuitEmptyComposer) => session.composer.delete(),
        Some(Action::Submit) => {
            let line = session.composer.text().to_owned();
            session.submit_line(&line);
        }
        Some(Action::CloseOverlayOrInterrupt | Action::Interrupt) => session.interrupt(),
        Some(Action::Newline) => session.composer.insert("\n"),
        Some(Action::AcceptCycleCompletion) => {
            if let Some(candidate) = session.candidates.first() {
                let completed = format!("/{} ", candidate.name);
                session.composer.set(&completed);
            } else {
                session.composer.insert("  ");
            }
        }
        Some(Action::Help) => session.pending_commands.push(Command::Run {
            name: "hotkeys".into(),
            args: "".into(),
            expected: None,
        }),
        Some(action) if apply_edit_action(&mut session.composer, action) => {}
        Some(Action::HistoryPrev) if session.composer.on_first_line() => {
            session.composer.history_prev();
        }
        Some(Action::HistoryPrev) => session.composer.move_up(),
        Some(Action::HistoryNext) if session.composer.on_last_line() => {
            session.composer.history_next();
        }
        Some(Action::HistoryNext) => session.composer.move_down(),
        None if key.code == KeyCode::Backspace => session.composer.backspace(),
        None if key.code == KeyCode::Delete => session.composer.delete(),
        None if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT => {
            if let KeyCode::Char(character) = key.code {
                session.composer.insert(character.encode_utf8(&mut [0; 4]));
            }
        }
        _ => {}
    }
    session.update_popup();
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::{ExitDialog, Session, apply_event};

    /// Applies one key to a fresh dialog and transcript on the inline screen.
    fn apply_key(session: &mut Session, key: crate::keys::Key) {
        use crate::keys::InputEvent;

        apply_event(
            session,
            &mut crate::dialog::DialogUi::default(),
            &crate::transcript::Transcript::default(),
            InputEvent::Key(key),
            false,
            crate::Screen::Inline,
        );
    }

    #[test]
    fn chooser_reply_is_not_rendered_as_debug_text() {
        let mut session = Session::default();
        assert_eq!(
            session.accept_reply(dal_core::Reply::Choose {
                chooser: dal_core::Chooser::ForkPoint,
                filter: "".into(),
            }),
            [] as [String; 0]
        );
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
        let mut composer = crate::composer::Composer::default();
        composer.set("rewrite the query");
        let mut session = Session {
            composer,
            ..Session::default()
        };
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        assert_eq!(session.dialog, Some(ExitDialog::DiscardDraft));
        assert!(!session.quit);
        session.answer_dialog('n');
        assert_eq!(session.dialog, None);
        assert!(!session.quit);
        assert_eq!(session.composer.text(), "rewrite the query");
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        session.answer_dialog('y');
        assert!(session.quit);
        assert_eq!(session.composer.text(), "");
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
    fn copy_reply_queues_the_text_for_the_clipboard_without_debug_rows() {
        let mut session = Session::default();
        let rows = session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::CopyReply {
            text: "the reply".into(),
        }));
        assert_eq!(rows.len(), 0);
        assert_eq!(session.pending_copies, vec!["the reply".into()]);
    }

    #[test]
    fn help_key_runs_the_hotkeys_command() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session::default();
        apply_key(&mut session, Key::new(KeyCode::F(1), KeyModifiers::NONE));
        assert!(matches!(
            &session.pending_commands[..],
            [dal_core::Command::Run { name, .. }] if name.as_ref() == "hotkeys"
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
}
// weave: run 'weave explain dal/crates/dal-tui/src/runtime.rs' for per-hunk detail, 'weave check' to verify your resolution
