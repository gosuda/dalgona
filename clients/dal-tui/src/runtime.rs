use crate::theme::ResolvedTheme;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use dal_agent::SessionRef;
use dal_agent::login::{Method, SecretString};
use dal_core::{
    Answer, BlobId, CancelScope, Chooser, ClientId, Command, Expect, FrontAction, Output, PageReq,
    Reply, RequestId, ResolutionCause, SessionId, Stop, TurnId, TurnState, UpdateKind,
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
    /// The draft the completion ring started from; every Tab/Shift+Tab
    /// applies a candidate to this base, so the ring never compounds
    /// prefixes. `None` once the draft changes or no candidates exist.
    popup_base: Option<String>,
    /// The draft the ring last wrote. Any user edit that diverges from this
    /// ends the ring, so the next Tab starts a fresh base instead of
    /// compounding the previous result.
    popup_last: String,
    /// The candidate the ring most recently accepted; the next Tab steps
    /// from here and wraps in both directions.
    popup_index: usize,
    /// Identity of the live session this terminal owns, for active-delete
    /// refusal. Seeded at attach; `None` only in unit harnesses that never
    /// attach.
    own: Option<dal_core::SessionId>,
    /// Resume target armed by the resume picker or `/resume`. The live loop
    /// opens it through public host ownership on its next pass, with this
    /// session still open: arming never sets `quit`, so the running turn is
    /// not cancelled and the screen is not torn down unless the open
    /// succeeds. The armed delete confirmation lives in the resume picker.
    resume_target: Option<ResumeRequest>,
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
    /// Set when Ctrl+G asked the loop to run the external editor on a
    /// temp copy of the draft.
    external_edit: bool,
    /// Set when Ctrl+L asked the loop to rewrite the whole live block.
    clear_region: bool,
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
    /// Start one sign-in method: a `/login` picker per-method row, or the
    /// method picker's choice. The API key still prompts for the key;
    /// every other method starts its flow directly.
    StartMethod {
        /// The provider id.
        provider: Box<str>,
        /// The method to start.
        method: Method,
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
    /// Delete one session tree through public host ownership after explicit
    /// two-step confirmation. The loop executes it after the input pass so
    /// the picker can disarm on any intervening key.
    DeleteSession(SessionId),
}

struct CommandContext<H> {
    specs: std::sync::Arc<[dal_core::CommandSpec]>,
    host: H,
}

/// A session the terminal was asked to reopen. The resume picker knows the
/// row's workspace; `/resume NAME` resolves an identity only, so for it the
/// workspace is looked up by id through the host before the open.
#[derive(Clone, Debug)]
struct ResumeRequest {
    id: dal_core::SessionId,
    workspace: Option<dal_core::Workspace>,
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
        // While a ring is live, any user edit diverges from the ring's own
        // output: end the ring so the next Tab starts a fresh one.
        if self.popup_base.is_some() && self.composer.text() != self.popup_last {
            self.popup_base = None;
            self.popup_index = 0;
            self.popup_last.clear();
        }
        self.popup.clear();
        self.candidates.clear();
        // While a ring is live the draft is the ring's own output, which no
        // longer completes names (`/mode ` is an argument draft), so the
        // candidates stay those of the base the ring started from.
        let draft = match &self.popup_base {
            Some(base) => base.as_str(),
            None => self.composer.text(),
        };
        match crate::popup::complete_draft(&self.commands, draft) {
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
            KeyAction::StartMethod(method) => {
                self.front.push(FrontRequest::StartMethod {
                    provider: signin.provider().into(),
                    method,
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
            Reply::Front(FrontAction::Resume { session }) => {
                if Some(session.id) == self.own {
                    vec!["This is the current session.".to_owned()]
                } else {
                    self.resume_target = Some(ResumeRequest {
                        id: session.id,
                        workspace: None,
                    });
                    Vec::new()
                }
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

    let mut model_source = model_source;
    let mut save_diagrams = save_diagrams;
    // Resume loop. A resume is attempted INSIDE the live loop (`run_loop`):
    // the replacement session is opened while this one is still open and
    // untouched, with its Surfaces, Painter and pump alive. A target that
    // cannot open (held by another process, gone, or one of this session's
    // own live sub-sessions, which closing this session would end) is
    // reported on the live screen and nothing else changes: the draft, the
    // history already printed, the running turn. Only an opened replacement
    // ends the pass, and only then is the old session's turn cancelled and
    // the session closed, after the terminal is restored. Only the final
    // pass writes the exit line, so a resume reopens silently and the user
    // sees one saved line. Delete never loops: it rebuilds the picker in
    // place.
    let mut agent = opts
        .rt
        .block_on(host.open(opts.session.clone(), ClientId::new("dal-tui")))?;
    let mut ephemeral = matches!(opts.session, SessionRef::Ephemeral { .. });
    let final_exit = loop {
        let session_id = agent.session();
        let (view, commands, mut pump, state, hook) = match attach_session(opts, io, host, &agent) {
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
                host: host.clone(),
            },
            &mut model_source,
            &mut save_diagrams,
            view,
            ephemeral,
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
            .and_then(|(_, turn, _, _)| *turn)
            .or_else(|| {
                opts.rt
                    .block_on(agent.view(snapshot_page()))
                    .ok()
                    .and_then(|view| match view.turn {
                        TurnState::Running { turn } => Some(turn),
                        _ => None,
                    })
            });
        if let Ok((_, _, _, Some(next))) = outcome {
            if let Err(error) = cancel_active(opts, &agent, io, &mut pump, running) {
                drop(pump);
                let _ = opts.rt.block_on(host.close(session_id));
                let _ = opts.rt.block_on(host.close(next.session()));
                return Err(error);
            }
            drop(pump);
            if let Err(error) = opts.rt.block_on(host.close(session_id)) {
                let _ = opts.rt.block_on(host.close(next.session()));
                return Err(error);
            }
            agent = next;
            // The session now open is a stored one, whatever the first was.
            ephemeral = false;
            continue;
        }
        let cancelled = cancel_active(opts, &agent, io, &mut pump, running);
        drop(pump);
        let close = opts.rt.block_on(host.close(session_id));
        let (exit, _, rows, _) = outcome?;
        cancelled?;
        close?;
        write_exit(io, opts, session_id, &exit, rows)?;
        break exit;
    };
    Ok(final_exit)
}

/// Everything one terminal pass owns once its session is attached: the
/// opening view, the command table, the delivery pump, the terminal-state
/// record, and the registration that lets the panic hook restore the terminal.
type Attached<S> = (
    dal_core::View,
    std::sync::Arc<[dal_core::CommandSpec]>,
    Pump<S>,
    Arc<Mutex<TermState>>,
    PanicHookGuard,
);

/// Reads the opening view, subscribes, and puts the terminal in raw mode for
/// one pass over `agent`.
fn attach_session<H: TuiHost>(
    opts: &TuiOptions,
    io: &dyn TermIo,
    host: &H,
    agent: &H::Agent,
) -> Result<Attached<<H::Agent as TuiAgent>::Subscription>, TuiError> {
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
    Ok((view, commands, pump, state, hook))
}

/// Lists the host's sessions as resume-picker rows, in the order the host
/// returns them. Each row carries the session's own workspace, so activating
/// it reopens the session where it lives, and the message count the listing
/// measured (the length of the session's visible leaf path); a row whose
/// journal could not be read has no count and says so.
fn resume_rows<H: TuiHost>(
    opts: &TuiOptions,
    host: &H,
) -> Result<Vec<crate::picker::SessionRow>, TuiError> {
    let page = opts.rt.block_on(host.sessions(dal_core::ListQuery {
        limit: Some(200),
        cursor: None,
        search: None,
    }))?;
    Ok(page
        .items
        .into_iter()
        .map(|info| {
            let name = info.name.as_deref().unwrap_or("(unnamed)");
            let when = info.updated_at.to_string();
            let dir = info.workspace.as_path().display().to_string();
            let label = crate::picker::resume_row(name, &when, info.message_count, &dir);
            crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: info.id,
                    workspace: info.workspace,
                },
                label,
            }
        })
        .collect())
}

/// Opens the session a resume request names, in the workspace it lives in.
/// The resume picker supplies the row's workspace. `/resume NAME` resolves an
/// identity only, so its workspace comes from the host's own uncapped
/// lookup by that id; a session the host cannot find is refused, never
/// opened in a guessed workspace. `leaving` is the session this terminal owns
/// and closes next; the target then opens as its successor, so the host
/// refuses a target that closing `leaving` would end (one of its own live
/// sub-sessions) before anything is cancelled or closed. With no owned
/// session (`None`) nothing is closed afterwards and the target opens plainly.
fn open_resume_target<H: TuiHost>(
    opts: &TuiOptions,
    host: &H,
    leaving: Option<dal_core::SessionId>,
    request: &ResumeRequest,
) -> Result<H::Agent, TuiError> {
    let workspace = match &request.workspace {
        Some(workspace) => workspace.clone(),
        None => {
            opts.rt
                .block_on(host.find_session(&request.id.to_string()))?
                .workspace
        }
    };
    let target = SessionRef::Resume {
        key: request.id.to_string().into(),
        workspace,
    };
    let client = ClientId::new("dal-tui");
    match leaving {
        Some(leaving) => opts
            .rt
            .block_on(host.open_replacing(leaving, target, client)),
        None => opts.rt.block_on(host.open(target, client)),
    }
}

/// Cancels a turn that outlived the loop, then reports it on inline screens.
///
/// `run_loop` parked the caret on the last row of the live block with
/// `Painter::rest_below_block`, so a leading CRLF steps below the block
/// without moving to or erasing the first terminal row (which can hold
/// committed transcript or prior shell output) and leaves the caret ready
/// for `write_exit`.
fn cancel_active<A: TuiAgent>(
    opts: &TuiOptions,
    agent: &A,
    io: &dyn TermIo,
    pump: &mut Pump<A::Subscription>,
    running: Option<TurnId>,
) -> Result<(), TuiError> {
    let Some(turn) = running else {
        return Ok(());
    };
    if let Err(error) = opts.rt.block_on(agent.submit(Command::Cancel {
        scope: CancelScope::Turn(turn),
    })) {
        return match opts.rt.block_on(agent.view(snapshot_page())) {
            Ok(view) if !matches!(view.turn, TurnState::Running { turn: active } if active == turn) => {
                Ok(())
            }
            _ => Err(error),
        };
    }
    if matches!(
        wait_for_cancelled(pump, turn, Duration::from_secs(3))?,
        Stop::Cancelled
    ) && !matches!(opts.screen, crate::Screen::Fullscreen)
    {
        let mut line = Vec::from([0x0d, 0x0a]);
        line.extend_from_slice(crate::copy::ids::TURN_CANCELLED.as_bytes());
        line.extend_from_slice(b"\x1b[0m");
        io.write(&line)
            .map_err(|error| crate::term::terminal_error(&error.to_string()))?;
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
            own: Some(agent.session()),
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
        view_arrived: true,
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
    surfaces
        .paint(io, state, &mut painter, opts, &palette)
        .map(|_| ())?;
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
    surfaces
        .paint(io, state, &mut painter, opts, &theme)
        .map(|_| ())?;
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

/// Turns on the interface modes a pass runs under, in the startup order:
/// bracketed paste, the alternate screen when fullscreen, then the one
/// startup probe whose replies the loop reads.
fn enter_interface_modes(
    io: &dyn TermIo,
    opts: &TuiOptions,
    state: &Arc<Mutex<TermState>>,
) -> Result<(), TuiError> {
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
        .map_err(|error| crate::term::te_raw_mode_failed(&error.to_string()))
}

type RunLoopOutcome<H> = (TuiExit, Option<TurnId>, Vec<String>, Option<H>);

fn run_loop<A, H, M, S>(
    io: &dyn TermIo,
    opts: &TuiOptions,
    state: &Arc<Mutex<TermState>>,
    agent: &A,
    pump: &mut Pump<A::Subscription>,
    commands: CommandContext<H>,
    model_source: &mut M,
    save_diagrams: &mut S,
    view: dal_core::View,
    ephemeral: bool,
) -> Result<RunLoopOutcome<H::Agent>, TuiError>
where
    A: TuiAgent,
    H: TuiHost,
    M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    S: FnMut(bool) -> Result<(), TuiError>,
{
    let CommandContext { specs, host } = commands;
    enter_interface_modes(io, opts, state)?;

    let (mut surfaces, mut painter, theme, probe, mut decoder) =
        boot(io, opts, state, agent, specs, view)?;
    let mut resolution_poll = Instant::now();
    let mut resumed = None;
    let mut dialog_landed = false;
    loop {
        let Some((events, term_width, term_height)) = poll_input(io, &mut decoder, &mut surfaces)?
        else {
            break;
        };
        let now = Instant::now();
        for event in events {
            apply_event_at(
                EventTarget {
                    session: &mut surfaces.session,
                    dialog: &mut surfaces.dialog,
                    transcript: &surfaces.transcript,
                    live: &mut surfaces.live,
                    entries: &surfaces.view.entries.items,
                    diagrams: surfaces.diagram_settings,
                    screen: opts.screen,
                    width: term_width,
                    height: term_height,
                },
                event,
                probe.kitty_keyboard,
            );
        }
        surfaces.flush_answers(opts, agent);
        surfaces.submit_commands(opts, agent, &host, model_source)?;
        surfaces.run_front(opts, &host, agent);
        // A closed expand view drops its sink; the read worker then has no
        // reason to exist and its guard is released here.
        if surfaces.live.expanded().is_none() {
            surfaces.session.blob_load = None;
        }
        surfaces.poll_signin(opts, io, model_source);
        poll_model_retry(&mut surfaces.session, model_source);
        if surfaces.session.external_edit {
            surfaces.session.external_edit = false;
            run_external_editor(opts, io, state, &mut surfaces);
            // The editor owned the screen: whatever it left must not survive
            // a diff paint that finds every row unchanged.
            painter.invalidate();
        }
        if surfaces.session.suspend {
            surfaces.session.suspend = false;
            suspend_and_continue(io, state, opts, &theme, &mut painter, &mut surfaces)?;
        }
        if std::mem::take(&mut surfaces.session.clear_region) {
            repaint_live_region(io, state, opts, &theme, &mut painter, &mut surfaces)?;
        }
        surfaces.write_copies(io);
        surfaces.apply_diagram_updates(opts, &mut painter, &probe, save_diagrams);
        surfaces.drain(opts, agent, pump)?;
        surfaces.poll_resolution(opts, agent, &mut resolution_poll);
        surfaces.reseed_diagrams(opts);
        // Arming runs only off a frame that landed with the dialog on
        // screen. A frame held while the caret measure is unanswered, or
        // below the usable floor, shows no question, so it resets the
        // delay below and no key can consent to an unseen question.
        if dialog_landed {
            surfaces.dialog.tick(Instant::now());
        }
        resumed = surfaces.open_armed_resume(opts, &host);
        if resumed.is_some() || surfaces.request_quit() {
            break;
        }
        surfaces.live.spin(now);
        dialog_landed = surfaces.paint(io, state, &mut painter, opts, &theme)?;
        if !dialog_landed
            && (!surfaces.dialog.is_open() || crate::render::below_floor(term_width, term_height))
        {
            surfaces.dialog.tick_without_dialog(Instant::now());
        }
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
        // An opened replacement is owned by nobody yet: close it here rather
        // than leave a live session no terminal will ever show.
        if let Some(next) = &resumed {
            let _ = opts.rt.block_on(host.close(next.session()));
        }
        return Err(error);
    }
    let (exit, running, rows) = surfaces.finish(ephemeral);
    Ok((exit, running, rows, resumed))
}

/// Reads one input batch, advancing the decoder and the tracked columns.
/// Returns `None` once shutdown is requested so the loop breaks.
fn poll_input(
    io: &dyn TermIo,
    decoder: &mut KeyDecoder,
    surfaces: &mut Surfaces,
) -> Result<Option<(Vec<InputEvent>, u16, u16)>, TuiError> {
    if io.shutdown_code().is_some() {
        return Ok(None);
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
        return Ok(None);
    }
    let now = Instant::now();
    let events = if bytes.is_empty() {
        decoder.tick(now)
    } else {
        decoder.feed(&bytes, now)
    };
    let (term_width, term_height) = io.size().unwrap_or((80, 24));
    Ok(Some((events, term_width, term_height)))
}

fn wait_for_cancelled<S: TuiSubscription>(
    pump: &mut Pump<S>,
    turn: TurnId,
    budget: Duration,
) -> Result<Stop, TuiError> {
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
                    return Ok(*stop);
                }
            }
            Ok(Ok(TuiDelivery::Resync(_))) => {
                return Err(TuiError::Terminal(
                    "Could not confirm turn cancellation: the session lost replay while waiting for confirmation.\nResume the session to check its state.".to_owned(),
                ));
            }
            Ok(Err(error)) => return Err(error),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(TuiError::Terminal(
                    "Could not confirm turn cancellation: the subscription closed while waiting for confirmation.\nResume the session to check its state.".to_owned(),
                ));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    Err(TuiError::Terminal(
        "Could not confirm turn cancellation: the turn gave no confirmation before the wait ran out.\nResume the session to check its state.".to_owned(),
    ))
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
    view_arrived: bool,
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

    /// Paints one frame over the surfaces' current state, returning whether
    /// the landed frame shows the dialog's request rows.
    fn paint(
        &mut self,
        io: &dyn TermIo,
        state: &Arc<Mutex<TermState>>,
        painter: &mut Painter,
        opts: &TuiOptions,
        theme: &ResolvedTheme,
    ) -> Result<bool, TuiError> {
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
    fn submit_commands<A, H, M>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        host: &H,
        model_source: &mut M,
    ) -> Result<(), TuiError>
    where
        A: TuiAgent,
        H: TuiHost,
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        if let Some(settled) = self.session.settled_run() {
            self.fold_submit_reply(opts, agent, host, model_source, settled)?;
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
            let pending_scope = match &command {
                Command::SetScopedModels(ids) => Some(ids.clone()),
                _ => None,
            };
            let submit_reply = opts.rt.block_on(agent.submit(command));
            let applied_scope = match &submit_reply {
                Ok(Reply::Done(_)) => pending_scope,
                _ => None,
            };
            self.fold_submit_reply(opts, agent, host, model_source, submit_reply)?;
            if let Some(ids) = applied_scope {
                self.view.settings.scoped_models = ids;
            }
        }
        Ok(())
    }

    /// Folds one host reply into a picker, transcript rows, or the session
    /// reply path.
    fn fold_submit_reply<A, H, M>(
        &mut self,
        opts: &TuiOptions,
        agent: &A,
        host: &H,
        model_source: &mut M,
        submit_reply: Result<Reply, TuiError>,
    ) -> Result<(), TuiError>
    where
        A: TuiAgent,
        H: TuiHost,
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
                    Chooser::ScopedModels => self.scoped_models_choice(model_source, &filter),
                    Chooser::Thinking => {
                        self.live
                            .notice(crate::copy::ids::PICKER_UNSUPPORTED.to_owned());
                        None
                    }
                    Chooser::Session => self.session_choice(opts, host, &filter),
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

    /// Builds the resume picker from the public host listing. Rows render
    /// through the existing `RESUME_ROW` deck entry; activation arms the
    /// open that the live loop attempts with this session still open. A
    /// failing listing keeps the failure visible instead of opening an empty
    /// picker.
    fn session_choice<H>(
        &mut self,
        opts: &TuiOptions,
        host: &H,
        filter: &str,
    ) -> Option<crate::picker::PickerUi>
    where
        H: TuiHost,
    {
        match resume_rows(opts, host) {
            Ok(rows) => Some(crate::picker::resume_picker(rows, filter)),
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!("note: cannot list sessions: {error}"),
                );
                None
            }
        }
    }

    fn scoped_models_choice<M>(
        &mut self,
        model_source: &mut M,
        filter: &str,
    ) -> Option<crate::picker::PickerUi>
    where
        M: FnMut() -> Result<Vec<ModelOption>, TuiError>,
    {
        if !self.view_arrived {
            self.live
                .notice(crate::copy::ids::PICKER_UNSUPPORTED.to_owned());
            return None;
        }
        let active = &self.view.settings.scoped_models;
        let rows = model_source()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|model| {
                model.scoped_id.map(|id| crate::picker::ScopedModelRow {
                    id,
                    label: model.label,
                })
            })
            .collect::<Vec<_>>();
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
                FrontRequest::StartMethod { provider, method } => {
                    self.session.picker = None;
                    self.session.signin = Some(if method == Method::ApiKey {
                        SignIn::key_prompt(&provider)
                    } else {
                        SignIn::flow(host, &opts.rt, &provider, method, None)
                    });
                }
                FrontRequest::LogoutPicker(filter) => self.logout_picker(opts, host, &filter),
                FrontRequest::Logout(provider) => self.logout(opts, host, Some(&provider)),
                FrontRequest::LogoutAll => self.logout(opts, host, None),
                FrontRequest::DeleteSession(id) => self.delete_session(opts, host, agent, id),
                FrontRequest::LoadBlobs(sink) => {
                    self.session.blob_load = Some(start_blob_load(&opts.rt, agent, &sink));
                }
            }
        }
    }

    /// Opens the sign-in overlay for `provider`: the key prompt, the flow,
    /// or the method choice when the provider offers several methods.
    fn begin_login<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H, provider: &str) {
        self.session.picker = None;
        let methods = signin::offered_methods(provider);
        self.session.signin = if methods.is_empty() {
            self.live.notice_as(
                crate::theme::Role::Error,
                format!(
                    "note: unknown provider \"{}\".",
                    crate::width::escape(provider)
                ),
            );
            None
        } else if methods.len() == 1 {
            Some(if methods[0] == Method::ApiKey {
                SignIn::key_prompt(provider)
            } else {
                SignIn::flow(host, &opts.rt, provider, methods[0], None)
            })
        } else {
            Some(SignIn::method_pick(provider, methods))
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

    /// Deletes one session tree after explicit confirmation. Refuses the
    /// live session and locked/owned trees without touching data; on success
    /// the resume picker rebuilds from a fresh listing so the removed row is
    /// gone (and keeps its other rows without it when that listing fails),
    /// otherwise the picker stays open on the failure notice.
    fn delete_session<H, A>(&mut self, opts: &TuiOptions, host: &H, agent: &A, id: SessionId)
    where
        H: TuiHost,
        A: TuiAgent,
    {
        if Some(id) == self.session.own || id == agent.session() {
            self.live.notice_as(
                crate::theme::Role::Error,
                "note: cannot delete the current session",
            );
            return;
        }
        match opts.rt.block_on(host.delete_session(id)) {
            Ok(()) => {
                self.live.notice_as(
                    crate::theme::Role::Text,
                    format!("note: deleted session {id}"),
                );
                self.session.picker = self.refresh_resume_picker(opts, host, id);
            }
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!("note: cannot delete session {id}: {error}"),
                );
            }
        }
    }

    /// Rebuilds the open resume picker from a fresh host listing after
    /// `deleted` was removed, keeping the current filter. Returns `None` when
    /// no picker is open. When the listing cannot be read the failure notice
    /// is shown and the picker keeps its rows minus the deleted session, so a
    /// row that no longer exists is never offered.
    fn refresh_resume_picker<H>(
        &mut self,
        opts: &TuiOptions,
        host: &H,
        deleted: SessionId,
    ) -> Option<PickerUi>
    where
        H: TuiHost,
    {
        let filter = self.session.picker.as_ref()?.filter().to_owned();
        if !self
            .session
            .picker
            .as_ref()
            .is_some_and(PickerUi::is_resume_picker)
        {
            return self.session.picker.take();
        }
        match resume_rows(opts, host) {
            Ok(rows) => Some(crate::picker::resume_picker(rows, &filter)),
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!("note: cannot list sessions: {error}"),
                );
                // The listing could not be re-read, so keep the rows the user
                // already has, minus the session just deleted: its row must
                // never be offered again.
                let mut picker = self.session.picker.take()?;
                picker.remove_resume_row(deleted);
                Some(picker)
            }
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

    /// Opens the session the resume picker or `/resume` armed, while this one
    /// stays open and untouched. An opened replacement is returned and ends
    /// the pass; a target that cannot open is reported as an error notice and
    /// the session carries on exactly as it was, running turn included. The
    /// target opens as the successor of the session this terminal owns, so a
    /// live child of it is refused by the host before anything is opened.
    fn open_armed_resume<H: TuiHost>(&mut self, opts: &TuiOptions, host: &H) -> Option<H::Agent> {
        let request = self.session.resume_target.take()?;
        match open_resume_target(opts, host, self.session.own, &request) {
            Ok(next) => Some(next),
            Err(error) => {
                self.live.notice_as(
                    crate::theme::Role::Error,
                    format!(
                        "note: cannot open session {}: {error}. This session stays open.",
                        request.id
                    ),
                );
                None
            }
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
    /// `ephemeral` is whether the session this pass ran was ephemeral: the
    /// caller knows it, because a resume replaces the session the options
    /// named with a stored one.
    fn finish(self, ephemeral: bool) -> (TuiExit, Option<TurnId>, Vec<String>) {
        let count = u64::try_from(self.view.entries.items.len()).unwrap_or(u64::MAX);
        let rows = self.transcript.rows().to_vec();
        let exit = TuiExit {
            session: Some(self.view.session.id),
            name: self.view.session.name,
            messages: count,
            ephemeral,
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
                EventTarget {
                    session: &mut surfaces.session,
                    dialog: &mut surfaces.dialog,
                    transcript: &surfaces.transcript,
                    live: &mut surfaces.live,
                    entries: &surfaces.view.entries.items,
                    diagrams: surfaces.diagram_settings,
                    screen: opts.screen,
                    width: probe_width,
                    height: probe_height,
                },
                &parser.finish(),
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
                EventTarget {
                    session: &mut surfaces.session,
                    dialog: &mut surfaces.dialog,
                    transcript: &surfaces.transcript,
                    live: &mut surfaces.live,
                    entries: &surfaces.view.entries.items,
                    diagrams: surfaces.diagram_settings,
                    screen: opts.screen,
                    width: probe_width,
                    height: probe_height,
                },
                &keys,
            );
            // A key event requests an immediate frame that bypasses the batch.
            surfaces
                .paint(io, state, painter, opts, palette)
                .map(|_| ())?;
        }
        if probe.da1 {
            let held = parser.finish();
            let (probe_width, probe_height) = io.size().unwrap_or((80, 24));
            apply_type_ahead_at(
                decoder,
                EventTarget {
                    session: &mut surfaces.session,
                    dialog: &mut surfaces.dialog,
                    transcript: &surfaces.transcript,
                    live: &mut surfaces.live,
                    entries: &surfaces.view.entries.items,
                    diagrams: surfaces.diagram_settings,
                    screen: opts.screen,
                    width: probe_width,
                    height: probe_height,
                },
                &held,
            );
            break;
        }
    }
    Ok(probe)
}

/// The surfaces one input event acts on, plus the screen geometry it was
/// measured at. Bundling these keeps the event entry points under the
/// argument-count lint without changing what any event can touch.
struct EventTarget<'a> {
    session: &'a mut Session,
    dialog: &'a mut DialogUi,
    transcript: &'a Transcript,
    live: &'a mut Live,
    entries: &'a [dal_core::EntryView],
    diagrams: crate::diagram::DiagramSettings,
    screen: crate::Screen,
    width: u16,
    height: u16,
}

/// Decodes terminal bytes into key events and applies them at the current
/// terminal size, so startup type-ahead below the usable floor fails closed
/// exactly like main-loop input: only Ctrl+C is admitted.
fn apply_type_ahead_at(
    decoder: &mut KeyDecoder,
    EventTarget {
        session,
        dialog,
        transcript,
        live,
        entries,
        diagrams,
        screen,
        width,
        height,
    }: EventTarget<'_>,
    bytes: &[u8],
) {
    if bytes.is_empty() {
        return;
    }
    for event in decoder.feed(bytes, Instant::now()) {
        apply_event_at(
            EventTarget {
                session: &mut *session,
                dialog: &mut *dialog,
                transcript,
                live: &mut *live,
                entries,
                diagrams,
                screen,
                width,
                height,
            },
            event,
            false,
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
    EventTarget {
        session,
        dialog,
        transcript,
        live,
        entries,
        diagrams,
        screen,
        width,
        height,
    }: EventTarget<'_>,
    event: InputEvent,
    kitty: bool,
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
    if let Some(mut confirm) = session.dialog.take() {
        match confirm.key(key) {
            Some(crate::dialog::DraftChoice::Discard) => session.discard_draft(),
            Some(crate::dialog::DraftChoice::Keep) => {
                // An answered Keep closes the question; the draft and
                // session stay exactly as they were.
                session.dialog = None;
            }
            None => session.dialog = Some(confirm),
        }
        return;
    }
    if dialog.is_open() {
        live.close_expand();
        if let Some(answer) = dialog.key(key) {
            session.pending_answers.push(answer);
        }
        return;
    }
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        session.interrupt_or_dismiss(live, transcript);
        return;
    }
    if session.overlay {
        apply_overlay_key(session, transcript, key, kitty);
        return;
    }
    if session.signin.is_some() {
        session.apply_signin_key(key);
        return;
    }
    if session.picker.is_some() {
        apply_picker_key(session, live, key);
        return;
    }
    if live.expanded().is_some() {
        apply_expand_key(live, key, kitty);
        return;
    }
    if session.help.is_some() {
        apply_help_key(session, key, kitty);
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

/// Steps the completion ring: Tab on no candidates keeps its two-space
/// contract. The first Tab of a fresh ring pins the base draft and takes
/// the top candidate (Shift+Tab the bottom); every later press steps the
/// ring from the last accepted index off that untouched base, so cycling
/// never compounds prefixes and wraps both ways. `update_popup` ends a ring
/// whose draft diverged from its own output, so an edit starts a fresh one.
fn accept_cycle_completion(session: &mut Session, key: crate::keys::Key) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let backward = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
    session.viewport.clear_selection();
    let count = session.candidates.len();
    if count == 0 {
        // No candidates: Tab keeps its plain-text contract of two
        // spaces, matching the documented empty-popup behavior.
        session.popup_base = None;
        session.popup_index = 0;
        session.popup_last.clear();
        session.composer.insert("  ");
        return;
    }
    // A fresh ring has accepted nothing: forward enters one step before
    // the first candidate, backward one step after the last (the seam
    // before the first). A live ring steps from the last accepted index.
    let (base, from) = match session.popup_base.take() {
        Some(base) => (base, session.popup_index),
        None => (
            session.composer.text().to_owned(),
            if backward { 0 } else { count - 1 },
        ),
    };
    let step = if backward {
        crate::popup::cycle_prev
    } else {
        crate::popup::cycle_next
    };
    let (index, draft) = step(&base, &session.candidates, from);
    if count == 1 {
        // The args hint shows for the accepted command and a second Tab
        // inserts its two spaces, as before the ring existed.
        session.popup_index = 0;
        session.popup_last.clear();
    } else {
        session.popup_base = Some(base);
        session.popup_last.clone_from(&draft);
        session.popup_index = index;
    }
    session.composer.set(&draft);
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
        Some(Action::AcceptCycleCompletion) => accept_cycle_completion(session, key),
        Some(Action::Help) => session.help = Some(HelpUi::default()),
        // Ctrl+Z parks the terminal through the same restore the exit path
        // uses, then raises SIGTSTP; the loop's resume path re-enters the
        // recorded modes and repaints. Windows leaves the key unbound.
        Some(Action::Suspend) => session.suspend = true,
        // Ctrl+G edits the draft in the configured editor; the loop runs the
        // child between frames so a blocked editor never starves the pump.
        Some(Action::ExternalEditor) => session.external_edit = true,
        // Ctrl+L clears the live region: the next paint rewrites the whole
        // block, which also repairs any rows a bidi payload mangled.
        Some(Action::ClearRegion) => session.clear_region = true,
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
        // Ctrl+Up / Ctrl+Down walk the submitted prompts at the boundary,
        // the same entries Up/Down walk, without moving the caret mid-draft.
        Some(Action::PrevPrompt) => {
            session.viewport.clear_selection();
            session.composer.history_prev();
        }
        Some(Action::NextPrompt) => {
            session.viewport.clear_selection();
            session.composer.history_next();
        }
        // Ctrl+R searches prompt history: the picker lists every submitted
        // user message, and Enter loads the row into the composer draft.
        Some(Action::HistorySearch) => {
            session.viewport.clear_selection();
            session.picker = Some(crate::picker::history_picker(entries, ""));
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
/// characters edit the filter. While the filter is empty the `j`/`k` move
/// keys, `d` (resume picker only), and Space take their documented
/// filter-free meanings: move, delete the focused row after confirmation,
/// and toggle a multi picker row. On a non-empty filter every unmodified
/// character, Space included, stays filter text; Ctrl+Space (0x00) toggles
/// from either state. PageUp/PageDown/Home/End and the Ctrl+P/N/B/F chords
/// resolve through the key table in both states, so bound picker keys never
/// go runtime-dead. `resolve_in` is mode-agnostic, so the kitty argument is
/// inert.
fn apply_picker_key(session: &mut Session, live: &mut Live, key: crate::keys::Key) {
    use crossterm::event::{KeyCode, KeyModifiers};

    let Some(picker) = session.picker.as_mut() else {
        return;
    };
    let table_action = resolve_in(key, false, &[Owner::Picker]);
    // The empty filter admits the documented plain-letter keys before the
    // filter-text arm; a non-empty filter sends every unmodified character
    // to the query, and Ctrl+Space (0x00) toggles from either state.
    let filter_empty = picker.filter().is_empty();
    let is_delete_key = key.code == KeyCode::Char('d')
        && key.modifiers == KeyModifiers::NONE
        && filter_empty
        && picker.is_resume_picker();
    if !is_delete_key {
        picker.disarm_delete();
    }
    if apply_picker_table_action(picker, key, table_action, filter_empty) {
        return;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => session.picker = None,
        (KeyCode::Enter, _) => {
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
                Some(PickerAction::Login { provider, method }) => {
                    session
                        .front
                        .push(FrontRequest::StartMethod { provider, method });
                    session.picker = None;
                }
                Some(PickerAction::SetDraft(text)) => {
                    // History recall: the row's message becomes the draft.
                    session.composer.set(&text);
                    session.popup_base = None;
                    session.popup_index = 0;
                    session.picker = None;
                    session.update_popup();
                }
                Some(PickerAction::ResumeSession(target)) => {
                    if Some(target.id) == session.own {
                        live.notice_as(
                            crate::theme::Role::Error,
                            "note: this is the current session",
                        );
                    } else {
                        session.resume_target = Some(ResumeRequest {
                            id: target.id,
                            workspace: Some(target.workspace),
                        });
                    }
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
        (KeyCode::Up, _) => picker.move_selection(false),
        (KeyCode::Down, _) => picker.move_selection(true),
        (KeyCode::Backspace, _) => picker.remove_filter_char(),
        (KeyCode::Char('r'), KeyModifiers::NONE) => apply_model_retry_key(session),
        (KeyCode::Char(' '), KeyModifiers::NONE) if filter_empty => {
            // picker.rs owns the contract: toggle_selected is a no-op on a
            // single picker, so unmodified Space never joins the filter on
            // an empty query and never invents a check where none exists.
            picker.toggle_selected();
        }
        (KeyCode::Char('j'), KeyModifiers::NONE) if filter_empty => {
            picker.move_selection(true);
        }
        (KeyCode::Char('k'), KeyModifiers::NONE) if filter_empty => {
            picker.move_selection(false);
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) if filter_empty => {
            apply_picker_delete_key(session, live);
        }
        (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            picker.add_filter_char(character);
        }
        _ => {}
    }
}

/// Applies the key-table picker actions: move, page, jump, and the guarded
/// toggle. Returns whether the key was one of them, so only keys the table
/// leaves alone reach the literal-key arms of `apply_picker_key`.
fn apply_picker_table_action(
    picker: &mut PickerUi,
    key: crate::keys::Key,
    action: Option<Action>,
    filter_empty: bool,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};

    let plain = key.modifiers == KeyModifiers::NONE;
    match action {
        Some(Action::PickerToggle) => {
            let is_ctrl_space =
                key.code == KeyCode::Char(' ') && key.modifiers.contains(KeyModifiers::CONTROL);
            if is_ctrl_space || filter_empty {
                picker.toggle_selected();
            } else {
                picker.add_filter_char(' ');
            }
        }
        Some(Action::PickerMoveUp) => {
            // Plain `k` moves only on an empty filter (DESIGN guarded
            // letters); on a non-empty filter it rejoins the query instead
            // of stealing filter text. Up and Ctrl+P always move.
            if !filter_empty && plain && key.code == KeyCode::Char('k') {
                picker.add_filter_char('k');
            } else {
                picker.move_selection(false);
            }
        }
        Some(Action::PickerMoveDown) => {
            // Plain `j` moves only on an empty filter; otherwise it filters.
            // Down and Ctrl+N always move.
            if !filter_empty && plain && key.code == KeyCode::Char('j') {
                picker.add_filter_char('j');
            } else {
                picker.move_selection(true);
            }
        }
        Some(Action::PickerPageUp) => {
            let step = picker.visible_range().len().max(1);
            for _ in 0..step {
                picker.move_selection(false);
            }
        }
        Some(Action::PickerPageDown) => {
            let step = picker.visible_range().len().max(1);
            for _ in 0..step {
                picker.move_selection(true);
            }
        }
        Some(Action::PickerFirst) => {
            let count = picker.visible_len();
            for _ in 0..count {
                if picker.is_selected(0) {
                    break;
                }
                picker.move_selection(false);
            }
        }
        Some(Action::PickerLast) => {
            let last = picker.visible_len().saturating_sub(1);
            for _ in 0..picker.visible_len() {
                if picker.is_selected(last) {
                    break;
                }
                picker.move_selection(true);
            }
        }
        _ => return false,
    }
    true
}

/// `d` on an empty filter, resume picker only (DESIGN delete-row contract);
/// every other picker lets `d` rejoin the filter. The first press arms an
/// explicit confirmation, the second press on the same row queues the host
/// delete, and the active session is refused without touching the tree.
fn apply_picker_delete_key(session: &mut Session, live: &mut Live) {
    let Some(picker) = session.picker.as_mut() else {
        return;
    };
    let Some(focused) = crate::picker::focused_resume_session(picker) else {
        picker.add_filter_char('d');
        return;
    };
    if Some(focused) == session.own {
        live.notice_as(
            crate::theme::Role::Error,
            "note: cannot delete the current session",
        );
        return;
    }
    if picker.armed_delete() == Some(focused) {
        session.front.push(FrontRequest::DeleteSession(focused));
        picker.disarm_delete();
    } else {
        picker.arm_delete(focused);
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
    surfaces.paint(io, state, painter, opts, theme).map(|_| ())
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

/// Runs the configured external editor on a temp copy of the draft and
/// reads the result back into the composer. A nonzero exit cancels the edit:
/// the draft stays untouched and no notice appears. A failure to prepare, start, or
/// read back the edit also leaves the draft untouched and surfaces an error
/// notice. A successful exit replaces the draft, checkpoints undo, puts the
/// caret at the end, and refreshes the completion state for the new draft.
/// The terminal restores before the child runs and re-enters the recorded
/// modes after, exactly like the suspend path.
fn run_external_editor(
    opts: &TuiOptions,
    io: &dyn TermIo,
    state: &Arc<Mutex<TermState>>,
    surfaces: &mut Surfaces,
) {
    let saved = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let restore = crate::term::restore_bytes(&saved);
    let run = (|| -> Result<Option<String>, String> {
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|error| format!("cannot create a temp file for the editor: {error}"))?;
        std::io::Write::write_all(&mut tmp, surfaces.session.composer.text().as_bytes())
            .map_err(|error| format!("cannot write the draft: {error}"))?;
        let path = tmp.path().to_path_buf();
        io.write(&restore)
            .map_err(|error| format!("cannot restore the terminal: {error}"))?;
        io.disable_raw();
        #[expect(
            clippy::disallowed_methods,
            reason = "the external editor is the terminal client's own process edge"
        )]
        let outcome = {
            let mut command = std::process::Command::new(opts.editor.as_ref());
            command.arg(&path);
            command
                .status()
                .map_err(|error| format!("cannot start the editor: {error}"))
        };
        // The terminal re-enters its modes whether or not the child ran, so
        // a missing editor never leaves the interface parked in cooked mode.
        reenter_modes(io, state, opts, &saved)?;
        let status = outcome?;
        if !status.success() {
            return Ok(None);
        }
        std::fs::read_to_string(&path)
            .map(Some)
            .map_err(|error| format!("cannot read the edited draft: {error}"))
    })();
    match run {
        Ok(Some(text)) => {
            surfaces.session.composer.set(&text);
            // The editor may have replaced a draft the completion ring wrote
            // or completed; the ring and its candidates follow the new draft.
            surfaces.session.update_popup();
        }
        Ok(None) => {}
        Err(message) => surfaces
            .live
            .notice_as(crate::theme::Role::Error, format!("note: {message}")),
    }
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

    use super::{EventTarget, Session, apply_event_at, poll_model_retry};

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
    fn apply_key(session: &mut Session, key: crate::keys::Key) -> crate::live::Live {
        use crate::keys::InputEvent;

        let mut live = crate::live::Live::default();
        apply_event_at(
            EventTarget {
                session,
                dialog: &mut crate::dialog::DialogUi::default(),
                transcript: &crate::transcript::Transcript::default(),
                live: &mut live,
                entries: &[],
                diagrams: crate::diagram::DiagramSettings::default(),
                screen: crate::Screen::Inline,
                width: 80,
                height: 24,
            },
            InputEvent::Key(key),
            false,
        );
        live
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
    fn tab_cycles_completion_candidates_and_shift_tab_walks_back() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let commands: Vec<dal_core::CommandSpec> = ["model", "mode", "moth"]
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
        session.composer.set("/m");
        session.update_popup();
        assert_eq!(session.candidates.len(), 3);

        // Candidates rank by name, whatever order the host listed them in:
        // mode, model, moth. The first Tab takes the top one, and each press
        // after it steps the same ring off the same `/m` base.
        let tab = Key::new(KeyCode::Tab, KeyModifiers::NONE);
        apply_key(&mut session, tab);
        assert_eq!(session.composer.text(), "/mode ");
        apply_key(&mut session, tab);
        assert_eq!(session.composer.text(), "/model ");
        apply_key(&mut session, tab);
        assert_eq!(session.composer.text(), "/moth ");
        apply_key(&mut session, tab);
        assert_eq!(session.composer.text(), "/mode ", "Tab wraps past the last");
        // Shift+Tab arrives as BackTab from a legacy terminal and as Tab plus
        // Shift under kitty; both walk back, wrapping past the first.
        apply_key(&mut session, Key::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "/moth ");
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(session.composer.text(), "/model ");
        apply_key(&mut session, tab);
        assert_eq!(session.composer.text(), "/moth ", "the ring turns around");
        // A fresh ring entered with Shift+Tab starts at the last candidate.
        session.composer.set("/m");
        session.update_popup();
        apply_key(&mut session, Key::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "/moth ");
        // No candidates: Tab inserts two spaces, the documented fallback.
        session.composer.set("plain text");
        session.update_popup();
        assert_eq!(
            session.candidates,
            [] as [crate::popup::Candidate; 0],
            "plain text must leave no completion candidates"
        );
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "plain text  ");
    }

    #[test]
    fn history_picker_enter_loads_the_message_into_the_draft() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let entries = vec![dal_core::EntryView {
            id: dal_core::EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: dal_core::EntryKind::User {
                parts: vec![dal_core::JournalPart::Text {
                    text: "fix the parser".into(),
                }],
            },
        }];
        let mut session = Session {
            picker: Some(crate::picker::history_picker(&entries, "")),
            ..Session::default()
        };
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "fix the parser");
        assert!(session.picker.is_none());
        // The completion ring state reset, so a later Tab starts fresh.
        assert_eq!(session.popup_base, None);
        assert_eq!(session.popup_index, 0);
    }

    #[test]
    fn recalling_a_prompt_drops_the_completions_of_the_draft_it_replaced() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let commands: Vec<dal_core::CommandSpec> = ["model", "mode"]
            .into_iter()
            .map(|name| dal_core::CommandSpec {
                name: dal_core::CommandName::parse(name).unwrap(),
                summary: format!("{name} summary").into(),
                args_hint: None,
            })
            .collect();
        let entries = vec![dal_core::EntryView {
            id: dal_core::EntryId::new(NonZeroU64::MIN),
            parent: None,
            kind: dal_core::EntryKind::User {
                parts: vec![dal_core::JournalPart::Text {
                    text: "fix the parser".into(),
                }],
            },
        }];
        let mut session = Session {
            commands: commands.into(),
            ..Session::default()
        };
        session.composer.set("/m");
        session.update_popup();
        assert_eq!(session.candidates.len(), 2);
        session.picker = Some(crate::picker::history_picker(&entries, ""));
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "fix the parser");
        // The recalled prompt completes nothing, so Tab is plain text again
        // instead of replacing the prompt with a command of the old draft.
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "fix the parser  ");
    }

    #[test]
    fn control_up_down_recall_prompts_and_control_r_opens_history() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session::default();
        session.composer.set("first");
        session.submit_line("first");
        session.composer.set("second");
        session.submit_line("second");

        apply_key(&mut session, Key::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(session.composer.text(), "second");
        apply_key(&mut session, Key::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(session.composer.text(), "first");
        apply_key(&mut session, Key::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(session.composer.text(), "second");
        apply_key(&mut session, Key::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(
            session.composer.text(),
            "",
            "past the newest, the draft returns"
        );

        // A submitted line leaves the composer empty; the draft Ctrl+R must
        // preserve is the one typed after it, which was never submitted.
        session.submit_line("typed draft");
        session.composer.set("another draft");
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            session.composer.text(),
            "another draft",
            "the unsent draft survives the search"
        );
        // The picker renders no rows without journal entries, but the
        // surface opens; Enter on a row loads it back into the draft.
        assert!(session.picker.is_some(), "Ctrl+R opens the history picker");
        // An unsent draft, typed rather than submitted: submitting empties
        // the composer.
        session.composer.set("typed draft");
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(
            session.composer.text(),
            "typed draft",
            "the draft survives the search"
        );
        // The picker renders no rows without journal entries, but the
        // surface opens; Enter on a row loads it back into the draft.
        assert!(session.picker.is_some(), "Ctrl+R opens the history picker");
        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.picker.is_none(), "Esc dismisses the history search");
        assert_eq!(
            session.composer.text(),
            "typed draft",
            "a dismissed search leaves the draft alone"
        );
    }

    #[test]
    fn control_l_sets_the_clear_region_flag_and_recalls_nothing() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut session = Session::default();
        session.composer.set("draft stays");
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        );
        assert!(session.clear_region, "Ctrl+L arms the repaint");
        assert_eq!(
            session.composer.text(),
            "draft stays",
            "Ctrl+L never edits the draft"
        );
        assert_eq!(session.pending_commands, []);
        assert_eq!(session.pending_answers, []);
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

    #[test]
    fn empty_filter_routes_j_k_space_and_d_before_filter_text() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let rows = vec![
            crate::picker::ScopedModelRow {
                id: "openai/gpt-6".into(),
                label: "GPT 6".into(),
            },
            crate::picker::ScopedModelRow {
                id: "anthropic/opus-4".into(),
                label: "Opus 4".into(),
            },
        ];
        let mut session = Session {
            picker: Some(crate::picker::scoped_models_picker(rows, &[], "")),
            ..Session::default()
        };
        // j moves down on the empty filter instead of joining the query.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(|picker| picker.is_selected(1)),
            "j moves on the empty filter"
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("")
        );
        // Space toggles the focused row on a multi picker.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(|picker| picker.is_checked(1)),
            "space checks the focused row"
        );
        // k moves back up.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(|picker| picker.is_selected(0)),
            "k moves on the empty filter"
        );
        // d joins the filter on a non-resume picker: no picker outside
        // the resume list promises deletion.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("d"),
            "d filters outside the resume picker"
        );
        // Backspace empties the query again, so the letters below start a
        // fresh one that the GPT row matches.
        apply_key(
            &mut session,
            Key::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("")
        );

        // Once the filter holds text, plain letters join the query again.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("g")
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gp")
        );
        // Ctrl+Space (0x00) toggles from a non-empty filter.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        );
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(|picker| picker.is_checked(0)),
            "ctrl+space toggles while the filter holds text"
        );
    }

    #[test]
    fn ctrl_p_n_move_in_both_filter_states_and_space_filters_when_nonempty() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let rows = || {
            vec![
                crate::picker::ScopedModelRow {
                    id: "openai/gpt-6".into(),
                    label: "GPT 6".into(),
                },
                crate::picker::ScopedModelRow {
                    id: "anthropic/opus-4".into(),
                    label: "Opus 4".into(),
                },
            ]
        };
        let mut session = Session {
            picker: Some(crate::picker::scoped_models_picker(rows(), &[], "")),
            ..Session::default()
        };
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        );
        assert!(
            session.picker.as_ref().is_some_and(|p| p.is_selected(1)),
            "ctrl+n moves on an empty filter"
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
        );
        assert!(
            session.picker.as_ref().is_some_and(|p| p.is_selected(0)),
            "ctrl+p moves on an empty filter"
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('o'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("o")
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        );
        assert!(
            session.picker.as_ref().is_some(),
            "ctrl+n never dismisses a non-empty picker"
        );
        let checked_before = session.picker.as_ref().is_some_and(|p| p.is_checked(0));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("o "),
            "space filters once text is present"
        );
        assert_eq!(
            session.picker.as_ref().map(|p| p.is_checked(0)),
            Some(checked_before),
            "space does not toggle on a non-empty filter"
        );
    }

    #[test]
    fn editing_the_draft_mid_cycle_starts_a_fresh_ring() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        // The `$` lane lists skills only: entries named `skill:<name>`.
        let commands: Vec<dal_core::CommandSpec> = ["skill:find-anything", "skill:find-files"]
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
        session.composer.set("$fi");
        session.update_popup();
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "$find-anything ");
        // Backspace edits the accepted draft into `$find-anything`, which
        // only the first skill still matches. A ring that outlived the edit
        // would step on from `$fi` to `$find-files`; a fresh ring completes
        // the edited draft instead.
        apply_key(
            &mut session,
            Key::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(session.composer.text(), "$find-anything");
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            session.composer.text(),
            "$find-anything ",
            "a fresh ring completes the edited draft"
        );
        // A ring of one ends on accept, so the next Tab is plain text again.
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "$find-anything   ");
    }

    #[test]
    fn accepting_the_only_candidate_shows_its_args_hint() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let commands = vec![dal_core::CommandSpec {
            name: dal_core::CommandName::parse("model").unwrap(),
            summary: "model summary".into(),
            args_hint: Some("provider/model".into()),
        }];
        let mut session = Session {
            commands: commands.into(),
            ..Session::default()
        };
        session.composer.set("/mod");
        session.update_popup();
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "/model ");
        assert_eq!(session.popup, ["/model · provider/model"]);
        apply_key(&mut session, Key::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(session.composer.text(), "/model   ");
    }

    #[test]
    fn populated_filter_letters_join_text_and_control_keys_navigate() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let rows = vec![
            crate::picker::ScopedModelRow {
                id: "openai/gpt-6".into(),
                label: "GPT 6".into(),
            },
            crate::picker::ScopedModelRow {
                id: "anthropic/opus-4".into(),
                label: "Opus 4".into(),
            },
        ];
        let mut session = Session {
            picker: Some(crate::picker::scoped_models_picker(rows, &[], "")),
            ..Session::default()
        };
        // Once the filter holds text, plain letters join the query again.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("g")
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gp")
        );
        // Ctrl+Space (0x00) toggles from a non-empty filter.
        apply_key(
            &mut session,
            Key::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        );
        assert!(
            session
                .picker
                .as_ref()
                .is_some_and(|picker| picker.is_checked(0)),
            "ctrl+space toggles while the filter holds text"
        );
        for character in ['j', 'k', ' '] {
            apply_key(
                &mut session,
                Key::new(KeyCode::Char(character), KeyModifiers::NONE),
            );
        }
        assert_eq!(
            session.picker.as_ref().map(crate::picker::PickerUi::filter),
            Some("gpjk ")
        );
        let picker = session.picker.as_mut().expect("picker stays open");
        while !picker.filter().is_empty() {
            picker.remove_filter_char();
        }
        for (character, selected) in [('n', 1), ('p', 0)] {
            apply_key(
                &mut session,
                Key::new(KeyCode::Char(character), KeyModifiers::CONTROL),
            );
            assert!(
                session
                    .picker
                    .as_ref()
                    .is_some_and(|picker| picker.is_selected(selected))
            );
        }
    }

    #[test]
    fn resume_picker_d_arms_confirms_and_refuses_the_live_session() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        use super::FrontRequest;

        let live = dal_core::SessionId::new_v7();
        let other = dal_core::SessionId::new_v7();
        let home = dal_core::Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let other_home = dal_core::Workspace::new(std::env::temp_dir().join("other"))
            .expect("absolute workspace");
        let rows = vec![
            crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: live,
                    workspace: home,
                },
                label: "live".to_owned(),
            },
            crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: other,
                    workspace: other_home,
                },
                label: "other".to_owned(),
            },
        ];
        let mut session = Session {
            picker: Some(crate::picker::resume_picker(rows, "")),
            own: Some(live),
            ..Session::default()
        };
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        let armed = |session: &Session| {
            session
                .picker
                .as_ref()
                .and_then(crate::picker::PickerUi::armed_delete)
        };
        assert!(armed(&session).is_none(), "live row never arms");
        assert!(session.front.is_empty(), "live row never queues a delete");
        apply_key(&mut session, Key::new(KeyCode::Down, KeyModifiers::NONE));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(armed(&session), Some(other), "first d arms");
        assert!(session.front.is_empty(), "first d never deletes");
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(
            matches!(
                session.front.as_slice(),
                [FrontRequest::DeleteSession(id)] if *id == other
            ),
            "second d queues the host delete"
        );
        assert!(armed(&session).is_none(), "the confirmation is spent");
        apply_key(&mut session, Key::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(session.picker.is_none(), "esc dismisses");
    }

    fn resume_rows_for(ids: &[dal_core::SessionId]) -> Vec<crate::picker::SessionRow> {
        let home = dal_core::Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        ids.iter()
            .map(|id| crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: *id,
                    workspace: home.clone(),
                },
                label: format!("row {id}"),
            })
            .collect()
    }

    /// An armed delete belongs to the picker opening that armed it. Ctrl+C
    /// closes the picker and takes the arming with it, so the same row in the
    /// next `/resume` needs a fresh first `d`: one press arms, none deletes.
    #[test]
    fn an_armed_delete_does_not_survive_ctrl_c_into_the_next_picker() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let own = dal_core::SessionId::new_v7();
        let target = dal_core::SessionId::new_v7();
        let ids = [target, own];
        let open = || Session {
            picker: Some(crate::picker::resume_picker(resume_rows_for(&ids), "")),
            own: Some(own),
            ..Session::default()
        };
        let mut session = open();
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(
            session
                .picker
                .as_ref()
                .and_then(crate::picker::PickerUi::armed_delete),
            Some(target),
            "the first d arms the focused row"
        );
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(session.picker.is_none(), "Ctrl+C closes the picker");

        // The next opening starts on the same row, unarmed.
        session.picker = Some(crate::picker::resume_picker(resume_rows_for(&ids), ""));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert!(
            session.front.is_empty(),
            "one d in a fresh picker never deletes: {:?}",
            session.front.len()
        );
        assert_eq!(
            session
                .picker
                .as_ref()
                .and_then(crate::picker::PickerUi::armed_delete),
            Some(target),
            "it only arms"
        );
    }

    /// Moving the focus or editing the filter abandons an armed delete, so a
    /// second `d` confirms only the row the first one named, unchanged.
    #[test]
    fn a_move_or_a_filter_edit_abandons_an_armed_delete() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let own = dal_core::SessionId::new_v7();
        let first = dal_core::SessionId::new_v7();
        let second = dal_core::SessionId::new_v7();
        let ids = [first, second, own];
        let mut session = Session {
            picker: Some(crate::picker::resume_picker(resume_rows_for(&ids), "")),
            own: Some(own),
            ..Session::default()
        };
        let armed = |session: &Session| {
            session
                .picker
                .as_ref()
                .and_then(crate::picker::PickerUi::armed_delete)
        };
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(armed(&session), Some(first));
        apply_key(&mut session, Key::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(armed(&session), None, "a focus move abandons it");
        apply_key(&mut session, Key::new(KeyCode::Up, KeyModifiers::NONE));
        apply_key(
            &mut session,
            Key::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        assert_eq!(armed(&session), Some(first));
        if let Some(picker) = session.picker.as_mut() {
            picker.paste("row");
        }
        assert_eq!(armed(&session), None, "a paste abandons it");
        assert!(session.front.is_empty(), "nothing was ever deleted");
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
                    ids.iter()
                        .map(std::convert::AsRef::as_ref)
                        .eq(["anthropic/vanished"]),
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

    /// Enter on a resume row arms the reopen with the row's own workspace and
    /// quits the loop; the row of the live session never arms it.
    #[test]
    fn resume_picker_enter_arms_the_row_with_its_workspace() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let live = dal_core::SessionId::new_v7();
        let other = dal_core::SessionId::new_v7();
        let home = dal_core::Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let other_home = dal_core::Workspace::new(std::env::temp_dir().join("other"))
            .expect("absolute workspace");
        let rows = vec![
            crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: live,
                    workspace: home,
                },
                label: "live".to_owned(),
            },
            crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: other,
                    workspace: other_home.clone(),
                },
                label: "other".to_owned(),
            },
        ];
        let mut session = Session {
            picker: Some(crate::picker::resume_picker(rows, "")),
            own: Some(live),
            ..Session::default()
        };
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(session.resume_target.is_none(), "the live row never arms");
        assert!(!session.quit);
        let rows = vec![crate::picker::SessionRow {
            target: crate::picker::ResumeTarget {
                id: other,
                workspace: other_home.clone(),
            },
            label: "other".to_owned(),
        }];
        session.picker = Some(crate::picker::resume_picker(rows, ""));
        apply_key(&mut session, Key::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            !session.quit,
            "the picked row arms the open, it does not quit the loop"
        );
        assert!(matches!(
            &session.resume_target,
            Some(super::ResumeRequest { id, workspace: Some(workspace) })
                if *id == other && *workspace == other_home
        ));
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
            EventTarget {
                session: &mut session,
                dialog: &mut crate::dialog::DialogUi::default(),
                transcript: &crate::transcript::Transcript::default(),
                live: &mut crate::live::Live::default(),
                entries: &[],
                diagrams: crate::diagram::DiagramSettings::default(),
                screen: crate::Screen::Inline,
                width: 80,
                height: 24,
            },
            InputEvent::Paste(b"ignored paste".to_vec()),
            false,
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

        use super::{EventTarget, Session, apply_event_at};
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
                EventTarget {
                    session,
                    dialog,
                    transcript,
                    live: &mut crate::live::Live::default(),
                    entries: &[],
                    diagrams: crate::diagram::DiagramSettings::default(),
                    screen: crate::Screen::Fullscreen,
                    width: 80,
                    height: 24,
                },
                InputEvent::Key(key),
                false,
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
                    EventTarget {
                        session: &mut session,
                        dialog: &mut dialog,
                        transcript: &transcript,
                        live: &mut crate::live::Live::default(),
                        entries: &[],
                        diagrams: crate::diagram::DiagramSettings::default(),
                        screen: crate::Screen::Inline,
                        width: 80,
                        height: 24,
                    },
                    InputEvent::Key(shift(code)),
                    false,
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
                EventTarget {
                    session: &mut session,
                    dialog: &mut dialog,
                    transcript: &transcript,
                    live: &mut crate::live::Live::default(),
                    entries: &[],
                    diagrams: crate::diagram::DiagramSettings::default(),
                    screen: crate::Screen::Inline,
                    width: 80,
                    height: 24,
                },
                InputEvent::Key(control_c()),
                false,
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

    /// A single unmodified character key for the floored-surface tests.
    fn char_key(c: char) -> crate::keys::Key {
        crate::keys::Key::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    /// An armed approval request so floored keys have a dialog to miss.
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

    /// Applies one event at `width`x`height` on the inline screen with
    /// fresh live, transcript, and diagram state.
    fn apply_at(
        session: &mut Session,
        dialog: &mut crate::dialog::DialogUi,
        event: crate::keys::InputEvent,
        width: u16,
        height: u16,
    ) {
        super::apply_event_at(
            super::EventTarget {
                session,
                dialog,
                transcript: &crate::transcript::Transcript::default(),
                live: &mut crate::live::Live::default(),
                entries: &[],
                diagrams: crate::diagram::DiagramSettings::default(),
                screen: crate::Screen::Inline,
                width,
                height,
            },
            event,
            false,
        );
    }

    fn floored_char_key(c: char) -> crate::keys::Key {
        crate::keys::Key::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    fn floored_approval(id: dal_core::RequestId) -> dal_core::Request {
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

    fn floored_apply_at(
        session: &mut Session,
        dialog: &mut crate::dialog::DialogUi,
        event: crate::keys::InputEvent,
        size: (u16, u16),
    ) {
        super::apply_event_at(
            super::EventTarget {
                session,
                dialog,
                transcript: &crate::transcript::Transcript::default(),
                live: &mut crate::live::Live::default(),
                entries: &[],
                diagrams: crate::diagram::DiagramSettings::default(),
                screen: crate::Screen::Inline,
                width: size.0,
                height: size.1,
            },
            event,
            false,
        );
    }

    fn check_floored_approval_flow() {
        use crate::dialog::DialogUi;
        use crate::keys::InputEvent;

        let mut session = Session::default();
        let mut dialog = DialogUi::default();
        let id = dal_core::RequestId::new_v7();
        dialog.opened(floored_approval(id));
        let start = std::time::Instant::now();
        dialog.tick(start);
        dialog.tick(start + crate::dialog::ARM_DELAY);
        assert!(dialog.armed());
        floored_apply_at(
            &mut session,
            &mut dialog,
            InputEvent::Key(floored_char_key('y')),
            (20, 5),
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
        floored_apply_at(
            &mut session,
            &mut dialog,
            InputEvent::Key(floored_char_key('y')),
            (80, 24),
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
    }

    fn check_floored_picker_flow() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut picker_session = Session {
            picker: Some(crate::picker::model_picker_failed("")),
            ..Session::default()
        };
        let mut no_dialog = DialogUi::default();
        floored_apply_at(
            &mut picker_session,
            &mut no_dialog,
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            (20, 5),
        );
        assert!(
            picker_session.pending_commands.is_empty(),
            "picker Enter below the floor must not activate"
        );
        assert!(
            picker_session.picker.is_some(),
            "the unseen picker stays open"
        );
        floored_apply_at(
            &mut picker_session,
            &mut no_dialog,
            InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            (80, 24),
        );
    }

    fn check_floored_composer_flow() {
        use crate::dialog::DialogUi;
        use crate::keys::InputEvent;

        let mut type_session = Session::default();
        let mut type_dialog = DialogUi::default();
        floored_apply_at(
            &mut type_session,
            &mut type_dialog,
            InputEvent::Key(floored_char_key('x')),
            (20, 5),
        );
        assert_eq!(
            type_session.composer.text(),
            "",
            "composer typing below the floor must not mutate the draft"
        );
        floored_apply_at(
            &mut type_session,
            &mut type_dialog,
            InputEvent::Paste(b"x".to_vec()),
            (20, 5),
        );
        assert_eq!(
            type_session.composer.text(),
            "",
            "paste below the floor must not reach the composer"
        );
    }

    fn check_floored_interrupt_flow() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut interrupt_session = Session {
            picker: Some(crate::picker::model_picker_failed("")),
            ..Session::default()
        };
        let mut interrupt_dialog = DialogUi::default();
        floored_apply_at(
            &mut interrupt_session,
            &mut interrupt_dialog,
            InputEvent::Key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            (20, 5),
        );
        assert!(
            interrupt_session.picker.is_none(),
            "Ctrl+C stays the interrupt path while floored"
        );
    }

    #[test]
    fn floored_surface_drops_blind_dialog_and_picker_keys() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

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
    }

    #[test]
    fn floored_surface_drops_blind_keys_until_resized_back() {
        assert!(crate::render::below_floor(20, 5));
        assert!(!crate::render::below_floor(80, 24));
        check_floored_approval_flow();
        check_floored_picker_flow();
        check_floored_composer_flow();
        check_floored_interrupt_flow();
    }

    #[test]
    fn floored_surface_drops_blind_typing_but_keeps_interrupt() {
        use crate::dialog::DialogUi;
        use crate::keys::{InputEvent, Key};
        use crossterm::event::{KeyCode, KeyModifiers};

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

    /// A named session resumes by the token `/name` prints for the same name:
    /// bare, double-quoted for spaces and apostrophes, or the session id.
    #[test]
    fn exit_line_resume_hint_is_the_resume_token() {
        for (name, hint) in [
            ("demo", "demo"),
            ("my-session_1.x", "my-session_1.x"),
            ("my session", "\"my session\""),
            ("o'brien", "\"o'brien\""),
            ("a $b", SESSION),
            ("demo$(calc)", SESSION),
            ("foo\"; calc #", SESSION),
        ] {
            let line = format!(
                "\r\ndal: session \"{name}\" saved · 1 message · run dal -r {hint} to resume\r\n"
            );
            assert_eq!(exit_output(Some(name)), line, "name {name:?}");
        }
    }

    #[test]
    fn exit_line_of_an_unnamed_session_resumes_by_id() {
        let line = format!(
            "\r\ndal: session \"{SESSION}\" saved · 1 message · run dal -r {SESSION} to resume\r\n"
        );
        assert_eq!(exit_output(None), line);
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
        assert_eq!(restored(&restorers), [] as [bool; 0]);
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

    /// Records every open and serves a fixed session listing, so the resume
    /// open can be checked without a terminal. `listing` is what the capped
    /// `sessions` call returns; `known` is what the uncapped `find_session`
    /// can resolve, which may hold sessions the listing cut off.
    #[derive(Clone)]
    struct ResumeHost {
        agent: FakeAgent,
        listing: Vec<SessionInfo>,
        known: Vec<SessionInfo>,
        opened: Arc<Mutex<Vec<SessionRef>>>,
        open_fails: bool,
        listing_fails: bool,
    }

    impl TuiHost for ResumeHost {
        type Agent = FakeAgent;

        fn open(
            &self,
            session: SessionRef,
            _client: ClientId,
        ) -> impl Future<Output = Result<FakeAgent, TuiError>> {
            self.opened
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session);
            std::future::ready(if self.open_fails {
                Err(TuiError::Terminal("the session is open elsewhere".into()))
            } else {
                Ok(self.agent.clone())
            })
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
                "the resume host has no providers".into(),
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

        fn sessions(
            &self,
            _query: dal_core::ListQuery,
        ) -> impl Future<Output = Result<dal_core::Page<SessionInfo, Box<str>>, TuiError>> {
            std::future::ready(if self.listing_fails {
                Err(TuiError::Terminal("the listing is unreadable".into()))
            } else {
                Ok(dal_core::Page {
                    items: self.listing.clone(),
                    next_before: None,
                })
            })
        }

        fn find_session(
            &self,
            reference: &str,
        ) -> impl Future<Output = Result<SessionInfo, TuiError>> {
            std::future::ready(
                self.known
                    .iter()
                    .find(|info| info.id.to_string() == reference)
                    .cloned()
                    .ok_or_else(|| {
                        TuiError::Terminal(format!("no session matches \"{reference}\""))
                    }),
            )
        }
    }

    fn listed_session(id: SessionId, workspace: &Workspace) -> SessionInfo {
        SessionInfo {
            id,
            name: None,
            preview: "".into(),
            workspace: workspace.clone(),
            updated_at: dal_core::Timestamp::UNIX_EPOCH,
            created_at: Some(dal_core::Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: None,
            message_count: Some(4),
        }
    }

    fn resume_host(listing: Vec<SessionInfo>) -> ResumeHost {
        ResumeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
            known: listing.clone(),
            listing,
            opened: Arc::new(Mutex::new(Vec::new())),
            open_fails: false,
            listing_fails: false,
        }
    }

    /// A host whose every open fails, as a session another process holds does.
    fn refusing_resume_host() -> ResumeHost {
        ResumeHost {
            open_fails: true,
            ..resume_host(Vec::new())
        }
    }

    fn opened_refs(host: &ResumeHost) -> Vec<SessionRef> {
        host.opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The picker names the row's own workspace, and the reopen resolves the
    /// session there, not in the workspace of the session being left.
    #[test]
    fn resume_opens_the_picked_session_in_its_own_workspace() {
        let rig = Rig::new();
        let id = SessionId::new_v7();
        let elsewhere = Workspace::new(std::env::temp_dir().join("elsewhere")).expect("absolute");
        let host = resume_host(Vec::new());
        let request = super::ResumeRequest {
            id,
            workspace: Some(elsewhere.clone()),
        };
        assert!(super::open_resume_target(&rig.opts, &host, None, &request).is_ok());
        let opened = opened_refs(&host);
        assert!(
            matches!(
                opened.as_slice(),
                [SessionRef::Resume { key, workspace }]
                    if **key == *id.to_string() && *workspace == elsewhere
            ),
            "the open carries the row's workspace: {opened:?}"
        );
    }

    /// `/resume NAME` resolves an identity only: its workspace comes from the
    /// host's own lookup by that id.
    #[test]
    fn resume_by_name_looks_the_workspace_up_by_id() {
        let rig = Rig::new();
        let id = SessionId::new_v7();
        let listed = Workspace::new(std::env::temp_dir().join("listed")).expect("absolute");
        let host = resume_host(vec![listed_session(id, &listed)]);
        let request = super::ResumeRequest {
            id,
            workspace: None,
        };
        assert!(super::open_resume_target(&rig.opts, &host, None, &request).is_ok());
        assert!(matches!(
            opened_refs(&host).as_slice(),
            [SessionRef::Resume { workspace, .. }] if *workspace == listed
        ));
    }

    /// A session far down the listing is outside the capped `sessions` page
    /// but not outside the host's uncapped lookup, so a direct resume still
    /// finds its workspace.
    #[test]
    fn resume_by_name_finds_a_session_beyond_the_listing_cap() {
        let rig = Rig::new();
        let id = SessionId::new_v7();
        let far = Workspace::new(std::env::temp_dir().join("far")).expect("absolute");
        let host = ResumeHost {
            known: vec![listed_session(id, &far)],
            ..resume_host(Vec::new())
        };
        assert!(host.listing.is_empty(), "the capped listing never names it");
        let request = super::ResumeRequest {
            id,
            workspace: None,
        };
        assert!(super::open_resume_target(&rig.opts, &host, None, &request).is_ok());
        assert!(matches!(
            opened_refs(&host).as_slice(),
            [SessionRef::Resume { workspace, .. }] if *workspace == far
        ));
    }

    /// A session the host cannot find is refused without any open: the
    /// terminal never opens it in a guessed workspace.
    #[test]
    fn resume_of_an_unknown_session_is_refused_without_an_open() {
        let rig = Rig::new();
        let host = resume_host(Vec::new());
        let request = super::ResumeRequest {
            id: SessionId::new_v7(),
            workspace: None,
        };
        assert!(super::open_resume_target(&rig.opts, &host, None, &request).is_err());
        assert!(opened_refs(&host).is_empty(), "nothing was opened");
    }

    /// A target that cannot open returns its error to the caller, which keeps
    /// the current session open (the outer loop closes it only after a
    /// replacement opened).
    #[test]
    fn a_resume_target_that_cannot_open_reports_the_error() {
        let rig = Rig::new();
        let host = refusing_resume_host();
        let request = super::ResumeRequest {
            id: SessionId::new_v7(),
            workspace: Some(Workspace::new(std::env::temp_dir()).expect("absolute")),
        };
        let error = super::open_resume_target(&rig.opts, &host, None, &request)
            .err()
            .expect("an open that fails is an error");
        assert!(error.to_string().contains("open elsewhere"));
        assert_eq!(opened_refs(&host).len(), 1, "one open was attempted");
    }

    /// The exit facts name the session the pass actually ran: a terminal that
    /// started ephemeral and resumed a stored session reports a stored one,
    /// whatever the startup options said.
    #[test]
    fn finish_reports_the_ephemeral_state_the_caller_names() {
        let ephemeral = Rig::new().surfaces.finish(true).0;
        assert!(ephemeral.ephemeral, "an ephemeral pass says so");
        let stored = Rig::new().surfaces.finish(false).0;
        assert!(
            !stored.ephemeral,
            "a pass that resumed a stored session is not ephemeral, even when the options name an ephemeral start"
        );
    }

    fn resume_summary(id: SessionId) -> dal_core::SessionSummary {
        dal_core::SessionSummary {
            id,
            name: Some("other".into()),
            message_count: 3,
            updated_at: dal_core::Timestamp::UNIX_EPOCH,
        }
    }

    /// Arming a resume (`/resume NAME`) never asks to quit, so the running
    /// turn is not cancelled: nothing is torn down until the replacement is
    /// open.
    #[test]
    fn arming_a_resume_does_not_cancel_the_running_turn() {
        let mut rig = Rig::new();
        let turn = dal_core::TurnId::new(NonZeroU64::MIN);
        rig.surfaces.session.active_turn = Some(turn);
        let target = SessionId::new_v7();
        let rows = rig
            .surfaces
            .session
            .accept_reply(Reply::Front(dal_core::FrontAction::Resume {
                session: resume_summary(target),
            }));
        assert_eq!(rows, [] as [String; 0]);
        assert!(!rig.surfaces.session.quit, "arming a resume never quits");
        assert!(
            !rig.surfaces.request_quit(),
            "no quit request, so no cancellation is queued"
        );
        assert!(
            rig.surfaces.session.pending_commands.is_empty(),
            "no Cancel was queued for the running turn"
        );
        assert_eq!(rig.surfaces.session.active_turn, Some(turn));
        assert!(rig.surfaces.session.resume_target.is_some());
    }

    /// A target that cannot open is reported inside the live screen and
    /// nothing else moves: the session stays open, the draft stays in the
    /// composer, the running turn is untouched, and no quit is requested. The
    /// Surfaces the user is looking at are the same ones that carry on.
    #[test]
    fn a_failed_resume_stays_inside_the_live_screen() {
        let mut rig = Rig::new();
        let turn = dal_core::TurnId::new(NonZeroU64::MIN);
        rig.surfaces.session.active_turn = Some(turn);
        rig.surfaces.session.composer.set("half-written reply");
        rig.surfaces.session.resume_target = Some(super::ResumeRequest {
            id: SessionId::new_v7(),
            workspace: Some(Workspace::new(std::env::temp_dir()).expect("absolute")),
        });
        let host = refusing_resume_host();
        let next = rig.surfaces.open_armed_resume(&rig.opts, &host);
        assert!(next.is_none(), "an unopened target ends nothing");
        assert!(
            rig.surfaces.session.resume_target.is_none(),
            "the request is spent"
        );
        assert!(!rig.surfaces.session.quit);
        assert_eq!(rig.surfaces.session.active_turn, Some(turn));
        assert_eq!(rig.surfaces.session.composer.text(), "half-written reply");
        let notices = rig.surfaces.live.notices();
        assert!(
            notices
                .iter()
                .any(|row| row.role == crate::theme::Role::Error
                    && row.text.contains("cannot open session")
                    && row.text.contains("This session stays open")),
            "the reason is on screen: {:?}",
            notices
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// An opened target is handed back to end the pass; no notice is shown.
    #[test]
    fn an_opened_resume_target_ends_the_pass() {
        let mut rig = Rig::new();
        rig.surfaces.session.resume_target = Some(super::ResumeRequest {
            id: SessionId::new_v7(),
            workspace: Some(Workspace::new(std::env::temp_dir()).expect("absolute")),
        });
        let host = resume_host(Vec::new());
        assert!(rig.surfaces.open_armed_resume(&rig.opts, &host).is_some());
        assert!(rig.surfaces.session.resume_target.is_none());
        assert_eq!(rig.surfaces.live.notices(), []);
    }

    /// When the listing cannot be re-read after a delete, the picker keeps its
    /// other rows but never offers the deleted session again.
    #[test]
    fn a_failed_refresh_after_a_delete_drops_the_deleted_row() {
        let mut rig = Rig::new();
        let gone = SessionId::new_v7();
        let kept = SessionId::new_v7();
        let home = Workspace::new(std::env::temp_dir()).expect("absolute");
        let row = |id| crate::picker::SessionRow {
            target: crate::picker::ResumeTarget {
                id,
                workspace: home.clone(),
            },
            label: format!("row {id}"),
        };
        rig.surfaces.session.picker =
            Some(crate::picker::resume_picker(vec![row(gone), row(kept)], ""));
        let host = ResumeHost {
            listing_fails: true,
            ..resume_host(Vec::new())
        };
        let picker = rig
            .surfaces
            .refresh_resume_picker(&rig.opts, &host, gone)
            .expect("the picker stays open on the rows it has");
        assert_eq!(picker.visible_len(), 1, "only the other session remains");
        assert!(
            picker.visible_option(0).is_some_and(|option| matches!(
                &option.action,
                crate::picker::PickerAction::ResumeSession(target) if target.id == kept
            )),
            "the remaining row is the kept session"
        );
        assert!(
            rig.surfaces
                .live
                .notices()
                .iter()
                .any(|row| row.text.contains("cannot list sessions")),
            "the listing failure is shown"
        );
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
            view_arrived: false,
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
        let (exit, _, _) = surfaces.finish(false);
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
        let opts = options(runtime.handle().clone());
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
    struct Ledger(Arc<Mutex<Vec<dal_core::EntryView>>>, Vec<Box<str>>);

    impl TuiAgent for Ledger {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            let mut view = idle_view();
            view.entries.items = self.0.lock().expect("ledger").clone();
            view.settings.scoped_models = self.1.clone();
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
        runtime: tokio::runtime::Runtime,
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
                view_arrived: false,
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
                runtime,
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

    /// `/new` cannot open a session in place: the refusal is an error notice
    /// with the `note:` prefix, never a plain transcript row.
    #[test]
    fn new_session_reply_is_an_error_notice() {
        let mut rig = Rig::new();
        let host = FakeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
        };
        let mut models = || Ok(Vec::new());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &rig.agent,
                &host,
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

    #[test]
    fn scoped_chooser_reply_opens_the_picker_on_the_agent_scope() {
        let mut rig = Rig::new();
        rig.agent.1 = vec!["openai/gpt-6".into(), "anthropic/opus-4".into()];
        let arrived = rig
            .runtime
            .block_on(rig.agent.view(super::snapshot_page()))
            .expect("the agent view arrives");
        rig.surfaces.view = arrived;
        rig.surfaces.view_arrived = true;
        let host = FakeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
        };
        let models = vec![
            crate::picker::ModelOption {
                label: "GPT 6".into(),
                command: dal_core::Command::SetModel {
                    model: dal_core::ModelRoute::Harness { id: "gpt-6".into() },
                    save: dal_core::command::Save::SessionOnly,
                },
                scoped_id: Some("openai/gpt-6".into()),
            },
            crate::picker::ModelOption {
                label: "Opus 4".into(),
                command: dal_core::Command::SetModel {
                    model: dal_core::ModelRoute::Harness {
                        id: "opus-4".into(),
                    },
                    save: dal_core::command::Save::SessionOnly,
                },
                scoped_id: Some("anthropic/opus-4".into()),
            },
        ];
        let mut source = || Ok(models.clone());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &rig.agent,
                &host,
                &mut source,
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
            .expect("the chooser opens the picker");
        assert!(picker.is_multi());
        assert_eq!(picker.visible_len(), 2);
        assert!(
            picker.is_checked(0),
            "the first committed id arrives checked"
        );
        assert!(
            picker.is_checked(1),
            "the second committed id arrives checked"
        );
        assert!(
            rig.surfaces.live.notices().is_empty(),
            "the supported picker posts no unsupported notice"
        );
    }

    #[derive(Clone)]
    struct ViewScopedAgent {
        view: View,
    }

    impl TuiAgent for ViewScopedAgent {
        type Subscription = Silent;

        fn session(&self) -> SessionId {
            SessionId::new_v7()
        }

        fn view(&self, _page: PageReq) -> impl Future<Output = Result<View, TuiError>> {
            std::future::ready(Ok(self.view.clone()))
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

    #[test]
    fn scoped_chooser_reply_opens_the_picker_on_the_remote_view_scope() {
        let mut rig = Rig::new();
        let mut view = idle_view();
        view.settings.scoped_models = vec!["openai/gpt-6".into(), "anthropic/opus-4".into()];
        let agent = ViewScopedAgent { view };
        let arrived = rig
            .runtime
            .block_on(agent.view(super::snapshot_page()))
            .expect("the remote view arrives");
        rig.surfaces.view = arrived;
        rig.surfaces.view_arrived = true;
        let host = FakeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
        };
        let models = vec![
            crate::picker::ModelOption {
                label: "GPT 6".into(),
                command: dal_core::Command::SetModel {
                    model: dal_core::ModelRoute::Harness { id: "gpt-6".into() },
                    save: dal_core::command::Save::SessionOnly,
                },
                scoped_id: Some("openai/gpt-6".into()),
            },
            crate::picker::ModelOption {
                label: "Opus 4".into(),
                command: dal_core::Command::SetModel {
                    model: dal_core::ModelRoute::Harness {
                        id: "opus-4".into(),
                    },
                    save: dal_core::command::Save::SessionOnly,
                },
                scoped_id: Some("anthropic/opus-4".into()),
            },
        ];
        let mut source = || Ok(models.clone());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &agent,
                &host,
                &mut source,
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
            .expect("the chooser opens the picker");
        assert!(picker.is_multi());
        assert_eq!(picker.visible_len(), 2);
        assert!(
            picker.is_checked(0),
            "the first view-carried id arrives checked"
        );
        assert!(
            picker.is_checked(1),
            "the second view-carried id arrives checked"
        );
        assert!(
            rig.surfaces.live.notices().is_empty(),
            "the view-carried scope posts no notice"
        );
    }

    #[test]
    fn scoped_chooser_reply_without_a_view_refuses_the_picker() {
        let mut rig = Rig::new();
        let host = FakeHost {
            agent: FakeAgent {
                log: Log::default(),
                run_entered: Arc::new(AtomicBool::new(false)),
            },
        };
        let models = vec![crate::picker::ModelOption {
            label: "GPT 6".into(),
            command: dal_core::Command::SetModel {
                model: dal_core::ModelRoute::Harness { id: "gpt-6".into() },
                save: dal_core::command::Save::SessionOnly,
            },
            scoped_id: Some("openai/gpt-6".into()),
        }];
        let mut source = || Ok(models.clone());
        rig.surfaces
            .fold_submit_reply(
                &rig.opts,
                &rig.agent,
                &host,
                &mut source,
                Ok(Reply::Choose {
                    chooser: dal_core::Chooser::ScopedModels,
                    filter: "".into(),
                }),
            )
            .expect("the chooser reply folds");
        assert!(
            rig.surfaces.session.picker.is_none(),
            "no view means no picker, never an empty scope"
        );
        let notices = rig.surfaces.live.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("not available"), "{notices:?}");
    }

    /// The plain text of every row one frame paints over the rig's surfaces,
    /// on a terminal big enough that no notice or picker row is budgeted away.
    fn frame_texts(rig: &Rig) -> Vec<String> {
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .expect("the palette theme loads");
        let surfaces = &rig.surfaces;
        let input = crate::render::FrameInput {
            view: &surfaces.view,
            screen: rig.opts.screen,
            composer: surfaces.session.composer.text(),
            cursor: surfaces.session.composer.cursor(),
            exit_draft: None,
            branch: None,
            popup: &surfaces.session.popup,
            live: &surfaces.live,
            dialog: &surfaces.dialog,
            picker: surfaces.session.picker.as_ref(),
            help: surfaces.session.help.as_ref(),
            signin: surfaces.session.signin.as_ref(),
            transcript: &surfaces.transcript,
            viewport: &surfaces.session.viewport,
            opts: &rig.opts,
            kitty_keyboard: false,
            theme: &theme,
            diagram_settings: surfaces.diagram_settings,
            diagram_cache: &surfaces.diagram_cache,
        };
        crate::render::frame_rows(input, 120, 30)
            .into_iter()
            .map(|row| row.text)
            .collect()
    }

    fn shows(rig: &Rig, text: &str) -> bool {
        frame_texts(rig).iter().any(|row| row.contains(text))
    }

    /// One key through the production input path, on the rig's own session
    /// and live region.
    fn press(rig: &mut Rig, key: crate::keys::Key) {
        super::apply_event_at(
            super::EventTarget {
                session: &mut rig.surfaces.session,
                dialog: &mut crate::dialog::DialogUi::default(),
                transcript: &rig.surfaces.transcript,
                live: &mut rig.surfaces.live,
                entries: &[],
                diagrams: crate::diagram::DiagramSettings::default(),
                screen: Screen::Inline,
                width: 80,
                height: 24,
            },
            crate::keys::InputEvent::Key(key),
            false,
        );
    }

    /// A resume picker over `ids`, first row focused.
    fn resume_picker_over(ids: &[SessionId]) -> crate::picker::PickerUi {
        let home = Workspace::new(std::env::temp_dir()).expect("absolute workspace");
        let rows = ids
            .iter()
            .map(|id| crate::picker::SessionRow {
                target: crate::picker::ResumeTarget {
                    id: *id,
                    workspace: home.clone(),
                },
                label: format!("row {id}"),
            })
            .collect();
        crate::picker::resume_picker(rows, "")
    }

    /// The line the copy deck says an armed delete of `id` shows.
    fn armed_line(id: SessionId) -> String {
        crate::copy::render(
            crate::copy::ids::RESUME_ARMED,
            &[("id", &id.to_string())],
            1,
        )
    }

    fn armed_row(rig: &Rig) -> Option<SessionId> {
        let picker = rig.surfaces.session.picker.as_ref();
        picker.and_then(crate::picker::PickerUi::armed_delete)
    }

    /// An armed resume-row delete is the picker's state and the frame draws
    /// it from there: on screen while the picker holds it, gone the moment any
    /// path ends it. Arming never writes the notices that outlive a picker,
    /// and every notice that was there stays.
    #[test]
    fn an_armed_delete_shows_while_the_picker_holds_it_and_never_after() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let own = SessionId::new_v7();
        let target = SessionId::new_v7();
        let line = armed_line(target);
        let plain = |code| Key::new(code, KeyModifiers::NONE);

        for how in ["ctrl+c", "esc", "move", "filter", "paste", "confirm"] {
            let mut rig = Rig::new();
            rig.surfaces.live.notice("kept notice");
            rig.surfaces.session.picker = Some(resume_picker_over(&[target, own]));
            rig.surfaces.session.own = Some(own);

            press(&mut rig, plain(KeyCode::Char('d')));
            assert_eq!(
                armed_row(&rig),
                Some(target),
                "{how}: the first d arms the focused row"
            );
            assert!(shows(&rig, &line), "{how}: the arming is on screen");

            match how {
                "ctrl+c" => press(
                    &mut rig,
                    Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ),
                "esc" => press(&mut rig, plain(KeyCode::Esc)),
                "move" => press(&mut rig, plain(KeyCode::Down)),
                "filter" => press(&mut rig, plain(KeyCode::Char('x'))),
                "confirm" => press(&mut rig, plain(KeyCode::Char('d'))),
                _ => super::apply_paste(
                    &mut rig.surfaces.session,
                    &mut crate::dialog::DialogUi::default(),
                    &rig.surfaces.live,
                    b"row",
                ),
            }

            assert_eq!(armed_row(&rig), None, "{how}: the arming is over");
            assert!(!shows(&rig, &line), "{how}: nothing of it stays on screen");
            assert!(
                shows(&rig, "kept notice"),
                "{how}: the other notice still shows"
            );
            let queued = matches!(
                rig.surfaces.session.front.as_slice(),
                [super::FrontRequest::DeleteSession(id)] if *id == target
            );
            assert_eq!(
                queued,
                how == "confirm",
                "{how}: only the second d queues the delete"
            );
        }
    }

    /// A picker that is replaced rather than closed takes its arming with
    /// it: a late chooser reply opens a fresh picker and a sign-in start
    /// drops the picker, and neither leaves the old arming on screen.
    #[test]
    fn a_replaced_picker_leaves_no_arming_on_screen() {
        use crate::keys::Key;
        use crossterm::event::{KeyCode, KeyModifiers};

        let own = SessionId::new_v7();
        let target = SessionId::new_v7();
        let line = armed_line(target);

        for how in ["late chooser reply", "sign-in start"] {
            let mut rig = Rig::new();
            let host = FakeHost {
                agent: FakeAgent {
                    log: Log::default(),
                    run_entered: Arc::new(AtomicBool::new(false)),
                },
            };
            let mut models = || Ok(Vec::new());
            rig.surfaces.live.notice("kept notice");
            rig.surfaces.session.picker = Some(resume_picker_over(&[target, own]));
            rig.surfaces.session.own = Some(own);
            press(&mut rig, Key::new(KeyCode::Char('d'), KeyModifiers::NONE));
            assert!(
                shows(&rig, &line),
                "{how}: armed and on screen before the replacement"
            );

            if how == "late chooser reply" {
                rig.surfaces
                    .fold_submit_reply(
                        &rig.opts,
                        &rig.agent,
                        &host,
                        &mut models,
                        Ok(Reply::Choose {
                            chooser: dal_core::Chooser::Settings,
                            filter: "".into(),
                        }),
                    )
                    .expect("the chooser reply folds");
                assert!(
                    rig.surfaces.session.picker.is_some(),
                    "{how}: a fresh picker opens"
                );
            } else {
                rig.surfaces.begin_login(&rig.opts, &host, "nonexistent");
                assert!(
                    rig.surfaces.session.picker.is_none(),
                    "{how}: the picker is gone"
                );
            }

            assert_eq!(armed_row(&rig), None, "{how}: the replacement is not armed");
            assert!(
                !shows(&rig, &line),
                "{how}: nothing of the old arming stays on screen"
            );
            assert!(
                shows(&rig, "kept notice"),
                "{how}: the earlier notice still shows"
            );
        }
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
                &host,
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

    /// A real host over a temporary store, started on `handle`'s runtime: no
    /// fake sits between the terminal and the session table.
    fn real_host(
        handle: &tokio::runtime::Handle,
        root: &std::path::Path,
    ) -> (dal_agent::Host, Workspace) {
        let data = root.join("data");
        let directory = root.join("workspace");
        std::fs::create_dir_all(&data).expect("data directory");
        std::fs::create_dir_all(&directory).expect("workspace directory");
        let workspace = Workspace::new(directory.clone()).expect("workspace");
        let config = dal_core::Config::load(dal_core::ConfigProduct::Dalgon, &data, "", Some(""))
            .expect("configuration");
        let host = handle
            .block_on(dal_agent::Host::start(
                dal_agent::Product {
                    name: "dal",
                    data_root: data,
                    defaults: "",
                    extensions: Vec::new(),
                    bundled: Vec::new(),
                },
                config,
                dal_agent::Env {
                    vars: std::collections::BTreeMap::new(),
                    cwd: directory,
                    sandbox_helper: None,
                },
            ))
            .expect("host");
        (host, workspace)
    }

    /// The boundary on a real host with no fake in the path. The session on
    /// screen owns a live child; resuming that child is refused on the live
    /// screen, the draft and the running turn stay as they were, the request
    /// is spent, and both sessions are still live. A live session outside the
    /// owned subtree still opens and survives the close of the session left
    /// behind, while that session's child ends with it.
    #[test]
    fn a_live_child_of_the_owned_session_is_refused_by_a_real_host() {
        let temp = tempfile::tempdir().expect("temporary store");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let handle = runtime.handle().clone();
        let (host, workspace) = real_host(&handle, temp.path());
        let client = ClientId::new("tui-resume");
        let open = |session: SessionRef| {
            handle
                .block_on(host.open(session, client.clone()))
                .expect("session opens")
        };
        let new_session = || SessionRef::New {
            workspace: workspace.clone(),
            name: None,
        };
        let root = open(new_session());
        let child = open(SessionRef::Child {
            parent: root.session(),
            call: dal_core::CallId::new("child"),
            workspace: workspace.clone(),
            name: None,
        });
        let other = open(new_session());
        let live = |agent: &dal_agent::Agent| handle.block_on(agent.poll_status()).is_ok();

        let opts = options(handle.clone());
        let mut rig = Rig::new();
        let turn = dal_core::TurnId::new(NonZeroU64::MIN);
        rig.surfaces.session.own = Some(root.session());
        rig.surfaces.session.active_turn = Some(turn);
        rig.surfaces.session.composer.set("half-written reply");
        rig.surfaces.session.resume_target = Some(super::ResumeRequest {
            id: child.session(),
            workspace: Some(workspace.clone()),
        });

        let next = rig.surfaces.open_armed_resume(&opts, &host);

        assert!(
            next.is_none(),
            "a live child of the owned session never opens"
        );
        assert!(
            rig.surfaces.session.resume_target.is_none(),
            "the request is spent"
        );
        assert!(!rig.surfaces.session.quit);
        assert_eq!(rig.surfaces.session.active_turn, Some(turn));
        assert_eq!(rig.surfaces.session.composer.text(), "half-written reply");
        let notices = rig.surfaces.live.notices();
        assert!(
            notices
                .iter()
                .any(|row| row.role == crate::theme::Role::Error
                    && row.text.contains("cannot open session")
                    && row.text.contains("This session stays open")),
            "the reason is on screen: {:?}",
            notices
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
        assert!(live(&root), "the session on screen stays live");
        assert!(live(&child), "the refused child stays live");

        rig.surfaces.session.resume_target = Some(super::ResumeRequest {
            id: other.session(),
            workspace: Some(workspace.clone()),
        });
        let successor = rig
            .surfaces
            .open_armed_resume(&opts, &host)
            .expect("a live session outside the owned subtree opens");
        assert_eq!(successor.session(), other.session());
        handle
            .block_on(host.close(root.session()))
            .expect("the outer loop closes the session it left");
        assert!(live(&successor), "the successor outlives the close");
        assert!(!live(&child), "the child ended with its owner");
    }

    /// Records every terminal write plus raw-mode transitions, so the
    /// external-editor tests prove the restore/re-enter ordering from bytes
    /// instead of trusting the code under test.
    struct ModeProbe {
        writes: Mutex<Vec<Vec<u8>>>,
        raw_enables: std::sync::atomic::AtomicUsize,
        raw_disables: std::sync::atomic::AtomicUsize,
    }

    impl ModeProbe {
        fn new() -> Self {
            Self {
                writes: Mutex::new(Vec::new()),
                raw_enables: std::sync::atomic::AtomicUsize::new(0),
                raw_disables: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn written(&self) -> Vec<u8> {
            self.writes.lock().expect("writes").concat()
        }

        /// Whether `reenter_modes` ran: raw mode re-entered and the
        /// bracketed-paste re-entry bytes pushed again.
        fn modes_reentered(&self) -> bool {
            self.raw_enables.load(Ordering::SeqCst) >= 1
                && contains(&self.written(), b"\x1b[?2004h")
        }

        /// Whether the restore bytes (paste off) precede the re-entry bytes
        /// (paste on): the child ran while the terminal was restored, and the
        /// modes came back after. A cooked-mode leak fails this.
        fn restored_before_reentered(&self) -> bool {
            let bytes = self.written();
            match (
                index_of(&bytes, b"\x1b[?2004l"),
                index_of(&bytes, b"\x1b[?2004h"),
            ) {
                (Some(off), Some(on)) => off < on,
                _ => false,
            }
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        index_of(haystack, needle).is_some()
    }

    fn index_of(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    impl TermIo for ModeProbe {
        fn enable_raw(&self) -> std::io::Result<()> {
            self.raw_enables.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn disable_raw(&self) {
            self.raw_disables.fetch_add(1, Ordering::SeqCst);
        }

        fn size(&self) -> std::io::Result<(u16, u16)> {
            Ok((80, 24))
        }

        fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.writes.lock().expect("writes").push(bytes.to_vec());
            Ok(())
        }

        fn read(&self, _timeout: Duration) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }

        fn raise_tstp(&self) {}
    }

    /// A nonzero editor exit cancels the edit: the draft and caret stay
    /// untouched and no notice appears, while the terminal modes still
    /// re-enter. Fails if the temp file is applied on the cancel path, if a
    /// notice is raised for a plain cancel, or if the cancel path returns
    /// before `reenter_modes` and parks the interface in cooked mode.
    #[test]
    fn external_editor_nonzero_exit_keeps_the_draft_and_reenters_modes() {
        let mut rig = Rig::new();
        rig.opts.editor = "false".into();
        rig.surfaces.session.composer.set("keep me");
        rig.surfaces.session.composer.move_left();
        rig.surfaces.session.composer.move_left();
        let caret = rig.surfaces.session.composer.cursor();
        assert!(caret < "keep me".len(), "the caret starts mid-draft");

        let io = ModeProbe::new();
        let state = Arc::new(Mutex::new(crate::term::TermState::new()));
        super::run_external_editor(&rig.opts, &io, &state, &mut rig.surfaces);

        assert_eq!(
            rig.surfaces.session.composer.text(),
            "keep me",
            "a cancelled edit must not touch the draft"
        );
        assert_eq!(
            rig.surfaces.session.composer.cursor(),
            caret,
            "a cancelled edit must not move the caret"
        );
        assert!(
            rig.surfaces.live.notices().is_empty(),
            "a cancel is silent: {:?}",
            rig.surfaces
                .live
                .notices()
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            io.restored_before_reentered(),
            "the terminal restores before the child and re-enters after"
        );
        assert!(io.modes_reentered(), "raw mode and paste re-enter");
    }

    /// A zero-exit editor replaces the draft with the file contents, moves
    /// the caret to the end, checkpoints undo, and refreshes the completion
    /// ring. Fails if the old draft survives a successful edit, if the caret
    /// stays mid-line, if undo cannot restore the pre-edit draft, or if the
    /// stale completion ring outlives the replaced draft.
    #[test]
    #[cfg(unix)]
    fn external_editor_success_replaces_the_draft_and_moves_the_caret_to_the_end() {
        let original = "printf 'edited by the editor' > \"$0.new\" && mv \"$0.new\" \"$0\"";
        let mut rig = Rig::new();
        rig.opts.editor = "/bin/sh".into();
        rig.surfaces.session.composer.set(original);
        rig.surfaces.session.popup_base = Some(original.into());
        rig.surfaces.session.popup_last = original.into();

        let io = ModeProbe::new();
        let state = Arc::new(Mutex::new(crate::term::TermState::new()));
        super::run_external_editor(&rig.opts, &io, &state, &mut rig.surfaces);

        assert_eq!(
            rig.surfaces.session.composer.text(),
            "edited by the editor",
            "a successful edit replaces the draft with the file contents; notices: {:?}",
            rig.surfaces.live.notices()
        );
        assert_eq!(
            rig.surfaces.session.composer.cursor(),
            "edited by the editor".len(),
            "the caret follows the replaced draft to its end"
        );
        assert!(
            rig.surfaces.session.popup_base.is_none(),
            "the completion ring restarts from the replaced draft"
        );
        rig.surfaces.session.composer.undo();
        assert_eq!(
            rig.surfaces.session.composer.text(),
            original,
            "the edit checkpoints, so undo restores the pre-edit draft"
        );
        assert!(
            rig.surfaces.live.notices().is_empty(),
            "a successful edit raises no notice"
        );
        assert!(io.modes_reentered(), "raw mode and paste re-enter");
    }

    /// An unspawnable editor still re-enters the terminal modes (no
    /// cooked-mode leak), keeps the draft and caret, and surfaces an error
    /// notice. Fails if the spawn-failure path returns before
    /// `reenter_modes`, or if the failure is swallowed silently. Needs no
    /// seam refactor: a bogus editor path makes the real spawn fail.
    #[test]
    fn external_editor_missing_binary_keeps_the_draft_reenters_modes_and_notices() {
        let mut rig = Rig::new();
        rig.opts.editor = "/nonexistent-dal-tui-editor-binary".into();
        rig.surfaces.session.composer.set("keep me");
        let caret = rig.surfaces.session.composer.cursor();

        let io = ModeProbe::new();
        let state = Arc::new(Mutex::new(crate::term::TermState::new()));
        super::run_external_editor(&rig.opts, &io, &state, &mut rig.surfaces);

        assert_eq!(
            rig.surfaces.session.composer.text(),
            "keep me",
            "a failed spawn must not touch the draft"
        );
        assert_eq!(
            rig.surfaces.session.composer.cursor(),
            caret,
            "a failed spawn must not move the caret"
        );
        assert!(
            io.restored_before_reentered(),
            "even a missing editor re-enters the modes: no cooked-mode leak"
        );
        assert!(io.modes_reentered(), "raw mode and paste re-enter");
        let notices = rig.surfaces.live.notices();
        assert_eq!(
            notices.len(),
            1,
            "the failure surfaces one notice: {notices:?}"
        );
        assert!(
            notices[0].text.contains("cannot start the editor"),
            "the notice names the cause: {}",
            notices[0].text
        );
        assert_eq!(
            notices[0].role,
            crate::theme::Role::Error,
            "a spawn failure is an error, not a dim note"
        );
    }

}
