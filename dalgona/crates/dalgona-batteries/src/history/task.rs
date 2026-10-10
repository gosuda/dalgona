// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! One session-owned dream task: idle timer, park policy, consolidation.
//!
//! The task owns its session's [`DreamState`] plus the turn-busy flag the
//! pure machine cannot see: hooks have no turn service in their inject list,
//! so `input` marks the turn busy and `settled` clears it. A timer that
//! fires mid-turn is deferred to the settled path, and consolidation never
//! runs while a turn is active.
//!
//! Open seams (owned by sibling parts, recorded in the node report): the
//! `dream.json` service name (`Name` rejects the dot, so the task uses
//! `dream` and the `Name`-to-path mapping belongs to the sidecar backend),
//! and letter body text for the judge summary (letter records carry ids,
//! digests, spans, and prior summaries, but no journal text, so the shared
//! context is assembled from what the records carry until the prompt skills
//! and letter part shares its text assembly).

mod consolidate;

use consolidate::BatchLetter;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use dal_agent::error::ServiceError;
use dal_agent::ext::{Caller, Services};
use dal_core::ext::{JobsOp, Name, SidecarOp};
use dal_core::{DenyReason, JobId, Notice, SessionId};
use dal_ext::judge::Judge;
use jiff::{Timestamp, ToSpan};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::{Instant, sleep};

use super::dream::{
    DreamAction, DreamEvent, DreamPhase, DreamState, check_dream_file, decode_dream_file,
    transition,
};
use super::{DREAM_SIDECAR_NAME, PROBE_INTERVAL_HOURS};

/// dream-job idle interval: 20 minutes without input.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "Duration::from_mins is not a stable const fn (rust#140881)"
)]
const IDLE_INTERVAL: Duration = Duration::from_secs(20 * 60);

/// Inbox messages for one session task.
pub(crate) enum TaskMsg {
    /// A first-prompt input arrived: mark busy, reset the idle timer.
    Input,
    /// An observe-only turn settled: clear busy, recount, maybe submit.
    Settled,
    /// The session ended: cancel the timer and any running job.
    End,
}

#[derive(Clone, Copy)]
enum JobMode {
    Normal,
    Probe,
}

impl JobMode {
    fn failure(self) -> DreamEvent {
        match self {
            Self::Normal => DreamEvent::JobTransientFailure,
            Self::Probe => DreamEvent::ProbeFailed,
        }
    }

    fn gate_off(self) -> DreamEvent {
        match self {
            Self::Normal => DreamEvent::JudgeOff,
            Self::Probe => DreamEvent::ProbeFailed,
        }
    }

    fn success(self) -> DreamEvent {
        match self {
            Self::Normal => DreamEvent::JobSucceeded,
            Self::Probe => DreamEvent::ProbeSucceeded,
        }
    }
}

/// Runs one session's dream loop until `End` or channel close.
pub(crate) async fn run(
    session: SessionId,
    services: Arc<dyn Services>,
    caller: Caller,
    mut rx: mpsc::Receiver<TaskMsg>,
) {
    let mut task = Task::new(session, services, caller);
    task.startup().await;
    let mut idle = Box::pin(sleep(IDLE_INTERVAL));
    loop {
        tokio::select! {
            message = task.next_message(&mut rx) => {
                let Some(message) = message else { return };
                match message {
                    TaskMsg::Input => {
                        task.on_input();
                        idle.as_mut().reset(Instant::now() + IDLE_INTERVAL);
                    }
                    TaskMsg::Settled => task.on_settled(&mut rx).await,
                    TaskMsg::End => {
                        task.on_end(&mut rx).await;
                        return;
                    }
                }
            }
            () = &mut idle => {
                if !task.busy {
                    task.on_timer(&mut rx).await;
                }
                idle.as_mut().reset(Instant::now() + IDLE_INTERVAL);
            }
        }
        if task.ended_flag {
            return;
        }
    }
}

struct Task {
    session: SessionId,
    services: Arc<dyn Services>,
    caller: Caller,
    state: DreamState,
    busy: bool,
    unavailable: bool,
    ended_flag: bool,
    stash: VecDeque<TaskMsg>,
    job: Option<JobId>,
    judge: Option<Judge>,
    batch: Vec<BatchLetter>,
    summaries: Vec<String>,
    last_consolidated: u64,
    dreams: u32,
    parked_at: Option<Timestamp>,
    last_probe_at: Option<Timestamp>,
}

impl Task {
    fn new(session: SessionId, services: Arc<dyn Services>, caller: Caller) -> Self {
        Self {
            session,
            services,
            caller,
            state: DreamState::new(),
            busy: false,
            unavailable: false,
            ended_flag: false,
            stash: VecDeque::new(),
            job: None,
            judge: None,
            batch: Vec::new(),
            summaries: Vec::new(),
            last_consolidated: 0,
            dreams: 0,
            parked_at: None,
            last_probe_at: None,
        }
    }

    /// Returns the next inbox message, replaying stashed arrivals first.
    async fn next_message(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) -> Option<TaskMsg> {
        if let Some(message) = self.stash.pop_front() {
            return Some(message);
        }
        rx.recv().await
    }

    /// Stashes channel arrivals; reports whether the session ended.
    fn note_end_arrived(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) -> bool {
        let mut ended = false;
        loop {
            match rx.try_recv() {
                Ok(TaskMsg::End) | Err(TryRecvError::Disconnected) => ended = true,
                Ok(other) => self.stash.push_back(other),
                Err(TryRecvError::Empty) => break,
            }
        }
        ended
    }

    /// Fires one machine event and executes its actions, tracking park entry.
    async fn fire(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, event: DreamEvent) {
        let was_parked = self.state.phase == DreamPhase::Parked;
        let actions = transition(&mut self.state, event);
        self.execute(rx, actions).await;
        if !was_parked && self.state.phase == DreamPhase::Parked {
            self.parked_at = Some(Timestamp::now());
        }
    }

    /// Loads `dream.json` and rebuilds counts from the letter records.
    async fn startup(&mut self) {
        let Ok(name) = Name::parse(DREAM_SIDECAR_NAME) else {
            self.unavailable = true;
            return;
        };
        match self
            .services
            .sidecar(&self.caller, SidecarOp::Read { name })
            .await
        {
            Err(ServiceError::Denied(DenyReason::Unavailable { what })) if &*what == "sidecar" => {
                self.unavailable = true;
            }
            Err(_) | Ok(None) => {}
            Ok(Some(bytes)) => self.apply_sidecar(&bytes),
        }
        self.recount().await;
    }

    /// Applies a decoded sidecar body; malformed state restarts from records.
    fn apply_sidecar(&mut self, bytes: &[u8]) {
        let Ok(file) = decode_dream_file(bytes) else {
            return;
        };
        if check_dream_file(&file, &self.session.to_string()).is_err() {
            return;
        }
        self.last_consolidated = file.last_consolidated_letter;
        if file.park.parked {
            self.state.phase = DreamPhase::Parked;
            self.state.park_notified = true;
        }
        self.state.identical_streak = file.park.identical_streak;
        self.state.transient_streak = file.park.transient_streak;
        self.state.last_failure = file.park.last_failure;
        self.last_probe_at = file
            .park
            .last_probe_at
            .as_deref()
            .and_then(|text| text.parse::<Timestamp>().ok());
    }

    fn on_input(&mut self) {
        self.busy = true;
        let actions = transition(&mut self.state, DreamEvent::Input);
        debug_assert_eq!(actions, vec![DreamAction::ResetTimer]);
    }

    /// Recounts from the records, then runs the settled transition.
    async fn on_settled(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        self.busy = false;
        self.recount().await;
        self.fire(rx, DreamEvent::Settled).await;
        self.maybe_probe(rx).await;
    }

    /// Runs the idle-timer transition; the caller defers while busy.
    async fn on_timer(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        self.fire(rx, DreamEvent::IdleTimer).await;
        self.maybe_probe(rx).await;
    }

    async fn on_end(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        let actions = transition(&mut self.state, DreamEvent::SessionEnd);
        self.execute(rx, actions).await;
    }

    /// Executes machine actions from the state machine.
    async fn execute(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, actions: Vec<DreamAction>) {
        for action in actions {
            if self.ended_flag {
                return;
            }
            match action {
                DreamAction::ResetTimer => {}
                DreamAction::SubmitJob => self.submit(rx).await,
                DreamAction::Cancel => self.cancel_job().await,
                DreamAction::Notify(text) => self.notify(text),
            }
        }
    }

    /// Starts one half-open probe when parked past the six-hour interval.
    async fn maybe_probe(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        if self.state.phase != DreamPhase::Parked || self.state.running {
            return;
        }
        let now = Timestamp::now();
        let parked_due = self
            .parked_at
            .is_none_or(|parked| parked + PROBE_INTERVAL_HOURS.hours() <= now);
        let probe_due = self
            .last_probe_at
            .is_none_or(|probe| probe + PROBE_INTERVAL_HOURS.hours() <= now);
        if !parked_due || !probe_due {
            return;
        }
        self.state.phase = DreamPhase::Probing;
        self.state.running = true;
        self.last_probe_at = Some(now);
        self.write_sidecar().await;
        self.submit_probe(rx).await;
    }

    async fn cancel_job(&mut self) {
        if let Some(id) = self.job.take() {
            let _ = self
                .services
                .jobs(&self.caller, JobsOp::Cancel { id })
                .await;
        }
    }

    fn notify(&self, text: String) {
        let notice = Notice {
            turn: None,
            kind: Box::from("history"),
            text: text.into(),
        };
        self.services.notify(&self.caller, notice);
    }
}
