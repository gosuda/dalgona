use crate::theme::ResolvedTheme;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_core::{
    Answer, CancelScope, Chooser, ClientId, Command, Expect, FrontAction, Output, PageReq, Reply,
    RequestId, Stop, TurnId, TurnState, UpdateKind,
};

use crate::backend::{TuiAgent, TuiDelivery, TuiHost, TuiSubscription};
use crate::dialog::DialogUi;
use crate::keys::{Action, InputEvent, KeyDecoder, Owner, resolve_in};
use crate::live::Live;
use crate::picker::{ModelOption, PickerAction, PickerUi};
use crate::render::{FrameInput, entry_rows_timed};
use crate::screen::driver::Painter;
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
    composer: String,
    dialog: Option<ExitDialog>,
    quit: bool,
    overlay: bool,
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
    cancelling: Option<TurnId>,
    cancel_settled: bool,
    cancel_deadline: Option<std::time::Instant>,
}

struct CommandContext<M> {
    specs: std::sync::Arc<[dal_core::CommandSpec]>,
    model_source: M,
}

impl Session {
    fn submit_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            self.composer.clear();
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
        self.composer.clear();
        self.update_popup();
    }

    fn update_popup(&mut self) {
        self.popup.clear();
        self.candidates.clear();
        match crate::popup::complete_draft(&self.commands, &self.composer) {
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
                if self.composer.trim().is_empty() {
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
            Reply::Front(action) => vec![crate::width::escape(&format!("{action:?}"))],
            Reply::Started(job) => vec![format!("Job {job} started.")],
            // A queued reply needs no row: the live block shows the queue.
            _ => Vec::new(),
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
    let view = opts.rt.block_on(agent.view(snapshot_page()?))?;
    let session_id = view.session.id;
    let subscription = opts
        .rt
        .block_on(agent.subscribe(Some((view.r#gen, view.seq))))?;
    let commands = opts.rt.block_on(host.commands())?;
    let mut pump = Pump::spawn(opts, subscription);
    let state = Arc::new(Mutex::new(TermState::new()));
    let hook = PanicHookGuard::install(Arc::clone(&state));
    io.enable_raw()
        .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))?;
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .set_raw(true);

    let outcome = run_loop(
        io,
        opts,
        &state,
        &agent,
        &mut pump,
        CommandContext {
            specs: commands,
            model_source,
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
        crate::copy::ids::EXIT_EPHEMERAL.to_owned()
    } else {
        // An unnamed session resumes by its id; a placeholder name resolves to nothing.
        let session = session_id.to_string();
        crate::copy::render(
            crate::copy::ids::EXIT_SAVED,
            &[
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
    let (probe, replay) = read_probe(io)?;
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
    for event in decoder.feed(&replay, Instant::now()) {
        apply_event(
            &mut surfaces.session,
            &mut surfaces.dialog,
            event,
            probe.kitty_keyboard,
        );
    }
    Ok((surfaces, painter, theme, probe, decoder))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the loop's inputs are the run's own locals; grouping adds a shell"
)]
fn run_loop<A, M, S>(
    io: &dyn TermIo,
    opts: &TuiOptions,
    state: &Arc<Mutex<TermState>>,
    agent: &A,
    pump: &mut Pump<A::Subscription>,
    commands: CommandContext<M>,
    mut save_diagrams: S,
    view: dal_core::View,
) -> Result<(TuiExit, Option<TurnId>, Vec<String>), TuiError>
where
    A: TuiAgent,
    M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    S: FnMut(bool) -> Result<(), TuiError>,
{
    let CommandContext {
        specs,
        mut model_source,
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
                event,
                probe.kitty_keyboard,
            );
        }
        surfaces.flush_answers(opts, agent);
        surfaces.submit_commands(opts, agent, &mut model_source)?;
        surfaces.write_copies(io);
        surfaces.apply_diagram_updates(opts, &mut painter, &probe, &mut save_diagrams);
        surfaces.drain(opts, agent, pump)?;
        surfaces.poll_resolution(opts, agent, &mut resolution_poll)?;
        surfaces.reseed_diagrams(opts);
        if surfaces.request_quit() {
            break;
        }
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

/// Owned delivery pump: one parked thread turns the async subscription into
/// a sync channel the 50 ms input loop drains without blocking.
///
/// `run` is called outside async context; the pump thread inherits that same
/// freedom and drives the subscription with the process-edge runtime handle.
/// The channel is intentionally unbounded: session deliveries are lossless
/// and must never shed. `Drop` stops the worker within one poll tick and
/// joins it, so neither shutdown nor resync leaks a thread.
struct Pump<S: TuiSubscription> {
    deliveries: std::sync::mpsc::Receiver<Result<TuiDelivery, TuiError>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    marker: std::marker::PhantomData<S>,
}

impl<S: TuiSubscription> Pump<S> {
    fn spawn(opts: &TuiOptions, mut subscription: S) -> Self {
        use std::sync::atomic::Ordering;
        let (sender, deliveries) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = std::thread::Builder::new()
            .name("dal-tui-delivery-pump".to_owned())
            .spawn({
                let runtime = opts.rt.clone();
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
                                if sender.send(Ok(delivery)).is_err() {
                                    break;
                                }
                            }
                            Ok(Err(error)) => {
                                let _ = sender.send(Err(error));
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
    fn restart(&mut self, opts: &TuiOptions, subscription: S) {
        *self = Self::spawn(opts, subscription);
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
                composer: &self.session.composer,
                popup: &self.session.popup,
                live: &self.live,
                dialog: &self.dialog,
                picker: self.session.picker.as_ref(),
                transcript: &self.transcript,
                opts,
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
                        pump.restart(opts, subscription);
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

    /// Submits every command the input map queued, folding the host reply into
    /// a picker, transcript rows, or the session reply path.
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
        for command in std::mem::take(&mut self.session.pending_commands) {
            if let Command::Cancel { .. } = command {
                self.session.cancelling = None;
            }
            let submit_reply = match opts.rt.block_on(agent.submit(command)) {
                Ok(submit_reply) => submit_reply,
                Err(error) => {
                    self.live.notice(error.to_string());
                    continue;
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
            if !reply_rows.is_empty() {
                let reply_rows: Vec<String> =
                    reply_rows.iter().map(|row| format!("  {row}")).collect();
                self.session.command_seq = self.session.command_seq.saturating_add(1);
                self.transcript.commit(
                    &format!("command-{}", self.session.command_seq),
                    &reply_rows,
                );
            }
        }
        Ok(())
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
        dal_core::EntryKind::ToolResult { call, .. } => transcript.tool_duration(call.as_str()),
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

fn read_probe(io: &dyn TermIo) -> Result<(crate::term::Probe, Vec<u8>), TuiError> {
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut parser = ReplyParser::default();
    let mut probe = crate::term::Probe::default();
    let mut replay = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            replay.extend(parser.finish());
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
        replay.extend(keys);
        if probe.da1 {
            break;
        }
    }
    Ok((probe, replay))
}

fn apply_event(session: &mut Session, dialog: &mut DialogUi, event: InputEvent, kitty: bool) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let InputEvent::Key(key) = event else {
        if let InputEvent::Paste(bytes) = event {
            if dialog.is_open() {
                dialog.paste(&bytes);
            } else if session.dialog.is_none() && session.picker.is_none() {
                session.composer.push_str(&String::from_utf8_lossy(&bytes));
                session.update_popup();
            }
        }
        return;
    };
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        if session.picker.take().is_none() {
            session.interrupt();
        }
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
    if session.picker.is_some() {
        apply_picker_key(session, key);
        return;
    }
    if session.overlay && key.code == KeyCode::Esc {
        session.overlay = false;
        return;
    }
    let action = resolve_in(key, kitty, &[Owner::App, Owner::Composer, Owner::Editor]);
    match action {
        Some(Action::QuitEmptyComposer) if session.composer.is_empty() => session.quit = true,
        Some(Action::Submit) => {
            let line = session.composer.clone();
            session.submit_line(&line);
        }
        Some(Action::CloseOverlayOrInterrupt | Action::Interrupt) => session.interrupt(),
        Some(Action::Newline) => session.composer.push('\n'),
        Some(Action::TranscriptOverlay) => session.overlay = !session.overlay,
        Some(Action::AcceptCycleCompletion) => {
            if let Some(candidate) = session.candidates.first() {
                session.composer = format!("/{} ", candidate.name);
            } else {
                session.composer.push_str("  ");
            }
        }
        Some(Action::Help) => session.pending_commands.push(Command::Run {
            name: "hotkeys".into(),
            args: "".into(),
            expected: None,
        }),
        None if key.code == KeyCode::Backspace => {
            crate::composer::pop_grapheme(&mut session.composer);
        }
        None if key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT => {
            if let KeyCode::Char(character) = key.code {
                session.composer.push(character);
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

    #[test]
    fn chooser_reply_is_not_rendered_as_debug_text() {
        let mut session = Session::default();
        assert!(
            session
                .accept_reply(dal_core::Reply::Choose {
                    chooser: dal_core::Chooser::ForkPoint,
                    filter: "".into(),
                })
                .is_empty()
        );
    }

    #[test]
    fn picker_arrow_enter_selects_and_escape_or_control_c_cancels() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
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
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Down, KeyModifiers::NONE)),
            false,
        );
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            false,
        );
        assert!(
            matches!(&session.pending_commands[..], [dal_core::Command::Run { args, .. }] if args.as_ref() == "openai/second")
        );
        assert!(session.picker.is_none());

        session.picker = Some(PickerUi::new("Pick", "", Vec::new()));
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Esc, KeyModifiers::NONE)),
            false,
        );
        assert!(session.picker.is_none());
        assert_eq!(session.pending_commands.len(), 1);

        session.picker = Some(PickerUi::new("Pick", "", Vec::new()));
        session.active_turn = Some(dal_core::TurnId::new(NonZeroU64::MIN));
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            false,
        );
        assert!(session.picker.is_none());
        assert_eq!(session.pending_commands.len(), 1);
    }

    #[test]
    fn settings_picker_updates_local_state_and_queues_config_saves() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session {
            picker: Some(crate::picker::settings_picker(false, "")),
            ..Session::default()
        };
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            false,
        );
        assert_eq!(
            session.pending_diagram_settings,
            [(true, dal_core::command::Save::SessionOnly)]
        );
        assert!(session.pending_commands.is_empty());
        assert!(session.picker.is_some());

        session
            .picker
            .as_mut()
            .expect("settings picker")
            .move_selection(true);
        apply_event(
            &mut session,
            &mut DialogUi::default(),
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            false,
        );
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
        let mut session = Session {
            composer: "rewrite the query".to_owned(),
            ..Session::default()
        };
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        assert_eq!(session.dialog, Some(ExitDialog::DiscardDraft));
        assert!(!session.quit);
        session.answer_dialog('n');
        assert_eq!(session.dialog, None);
        assert!(!session.quit);
        assert_eq!(session.composer, "rewrite the query");
        session.accept_reply(dal_core::Reply::Front(dal_core::FrontAction::Quit));
        session.answer_dialog('y');
        assert!(session.quit);
        assert!(session.composer.is_empty());
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
        assert!(rows.is_empty());
        assert_eq!(session.pending_copies, vec!["the reply".into()]);
    }

    #[test]
    fn help_key_runs_the_hotkeys_command() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session::default();
        let mut dialog = DialogUi::default();
        apply_event(
            &mut session,
            &mut dialog,
            InputEvent::Key(Key::new(KeyCode::F(1), KeyModifiers::NONE)),
            false,
        );
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
        assert!(session.composer.is_empty());
    }
}
