//! The runtime adapter: [`WatchFactory`] and [`StreamWatch`] over a TTSR watch.
//!
//! The driver loop starts one [`TtsrWatchAdapter`] per model request through
//! [`TtsrWatchFactory`], feeds stream deltas inline, and maps the first new
//! `Interrupt` fire of a call to [`StreamVerdict::Interrupt`]. Reminder and
//! report fires stay in [`TtsrWatchAdapter::fires`] for the driver to record
//! through [`TtsrWatchAdapter::record_body`] and
//! [`TtsrWatchAdapter::gate_record`] once its batch is durable.
//!
//! The budget follows the turn's live interrupt count: `Interrupts` while
//! the count is below `max_retries`, else `RemindersOnly`. The factory owns
//! the counter and resets it when the turn changes, so no reset protocol
//! with the loop is needed. Judged matches record fires but never stop the
//! sync watch and never surface as trait interrupts; their bool verdict
//! gating belongs to the judged-lane consumer on the record path.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dal_agent::ext::{
    StreamFire, StreamFireAction, StreamWatch, StreamWatchRecord, TurnInfo, WatchFactory,
};
use dal_core::{Channel, EntryId, RawJson, RulesConfig, StreamVerdict, TurnId};

use super::super::build::RuleSet;
use super::super::gate::Gate;
use super::super::readers::EditStyle;
use super::super::value::RuleAction;
use super::{Fire, Watch, WatchBudget, WatchError, create};

/// The turn a budget count belongs to, with its delivered interrupts.
#[derive(Debug)]
struct TurnBudget {
    turn: Option<TurnId>,
    interrupts: u32,
}

/// Builds one [`TtsrWatchAdapter`] per model request from a shared rule
/// snapshot and repeat gate.
pub struct TtsrWatchFactory {
    set: Arc<RuleSet>,
    gate: Arc<Mutex<Gate>>,
    cfg: RulesConfig,
    ws_root: PathBuf,
    edit_style: EditStyle,
    budget: Arc<Mutex<TurnBudget>>,
}

impl TtsrWatchFactory {
    /// Shares `set`, `gate`, and the turn interrupt budget across every
    /// request this factory watches. The caller builds `set` once with
    /// [`super::super::build::set_for`].
    pub fn new(
        set: Arc<RuleSet>,
        gate: Arc<Mutex<Gate>>,
        cfg: RulesConfig,
        ws_root: PathBuf,
        edit_style: EditStyle,
    ) -> Self {
        Self {
            set,
            gate,
            cfg,
            ws_root,
            edit_style,
            budget: Arc::new(Mutex::new(TurnBudget {
                turn: None,
                interrupts: 0,
            })),
        }
    }
}

impl WatchFactory for TtsrWatchFactory {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        let mut budget = self.budget.lock().ok()?;
        if budget.turn != Some(turn.turn) {
            budget.turn = Some(turn.turn);
            budget.interrupts = 0;
        }
        let watch_budget = if budget.interrupts < self.cfg.max_retries {
            WatchBudget::Interrupts
        } else {
            WatchBudget::RemindersOnly
        };
        let gate = self.gate.lock().ok()?;
        let watch = create(
            &self.set,
            &gate,
            turn.turn,
            watch_budget,
            &self.cfg,
            &self.ws_root,
            self.edit_style,
        );
        Some(Box::new(TtsrWatchAdapter {
            watch,
            gate: Arc::clone(&self.gate),
            turn: turn.turn,
            max_retries: self.cfg.max_retries,
            budget: Arc::clone(&self.budget),
            stopped: false,
            reported_fires: 0,
        }))
    }
}

/// One request's TTSR watch behind the runtime watcher contract.
pub struct TtsrWatchAdapter {
    watch: Watch,
    gate: Arc<Mutex<Gate>>,
    turn: TurnId,
    max_retries: u32,
    budget: Arc<Mutex<TurnBudget>>,
    stopped: bool,
    reported_fires: usize,
}

impl TtsrWatchAdapter {
    /// The turn this adapter watches.
    #[must_use]
    pub fn turn(&self) -> TurnId {
        self.turn
    }

    /// Every fire in delivery order, including earlier reminders.
    #[must_use]
    pub fn fires(&self) -> &[Fire] {
        self.watch.fires()
    }

    /// Builds the journal payload for one fire; see [`Watch::record_body`].
    #[must_use]
    pub fn record_body(
        &self,
        fire: usize,
        at: jiff::Timestamp,
        entry: Option<EntryId>,
    ) -> Box<str> {
        self.watch.record_body(fire, at, entry)
    }

    /// Latches one delivered fire in the shared repeat gate; see
    /// [`Watch::gate_record`]. A poisoned gate skips the latch.
    pub fn gate_record(&self, fire: usize, entry: Option<EntryId>) {
        if let Ok(mut gate) = self.gate.lock() {
            self.watch.gate_record(&mut gate, fire, entry);
        }
    }

    fn interrupt_verdict(&mut self, before: usize) -> StreamVerdict {
        let fired = self
            .watch
            .fires()
            .iter()
            .skip(before)
            .find(|fire| fire.action == RuleAction::Interrupt && !fire.judged)
            .map(|fire| {
                debug_assert!(
                    fire.inject.is_some(),
                    "interrupt fires always carry inject text"
                );
                (
                    fire.rule.as_str().into(),
                    fire.inject.clone().unwrap_or_default(),
                )
            });
        let Some((rule, inject)) = fired else {
            return StreamVerdict::Continue;
        };
        // A poisoned budget degrades rather than interrupting.
        let Ok(mut budget) = self.budget.lock() else {
            return StreamVerdict::Continue;
        };
        if budget.turn == Some(self.turn) && budget.interrupts < self.max_retries {
            budget.interrupts += 1;
            StreamVerdict::Interrupt { rule, inject }
        } else {
            // At the cap the inner watch keeps matching as reminders
            // instead of stopping on an undeliverable interrupt.
            self.watch.set_budget(WatchBudget::RemindersOnly);
            StreamVerdict::Continue
        }
    }

    fn stop_verdict(&mut self, before: usize) -> StreamVerdict {
        let verdict = self.interrupt_verdict(before);
        self.stopped = matches!(verdict, StreamVerdict::Interrupt { .. });
        verdict
    }
}

impl StreamWatch for TtsrWatchAdapter {
    fn feed(&mut self, channel: Channel, delta: &str) -> StreamVerdict {
        if self.stopped {
            return StreamVerdict::Continue;
        }
        let source = match channel {
            Channel::Thinking => super::SourceKind::Thinking,
            Channel::ToolArgs { tool } => super::SourceKind::Tool {
                tool: tool.as_str().into(),
            },
            // The core enum is non-exhaustive; an unknown future channel
            // feeds the assistant-visible matcher.
            _ => super::SourceKind::Text,
        };
        let before = self.watch.fires().len();
        match self.watch.feed(source, delta) {
            Ok(super::WatchVerdict::Stop) => self.stop_verdict(before),
            Ok(super::WatchVerdict::Continue) => StreamVerdict::Continue,
            Err(WatchError::WatchStopped) => {
                self.stopped = true;
                StreamVerdict::Continue
            }
        }
    }

    fn finish(&mut self) -> StreamVerdict {
        if self.stopped {
            return StreamVerdict::Continue;
        }
        let before = self.watch.fires().len();
        match self.watch.finish() {
            Ok(super::WatchVerdict::Stop) => self.stop_verdict(before),
            Ok(super::WatchVerdict::Continue) => StreamVerdict::Continue,
            Err(WatchError::WatchStopped) => {
                self.stopped = true;
                StreamVerdict::Continue
            }
        }
    }

    fn take_fires(&mut self) -> Vec<StreamFire> {
        let fires = self.watch.fires();
        let start = self.reported_fires;
        self.reported_fires = fires.len();
        fires
            .iter()
            .enumerate()
            .skip(start)
            .map(|(index, fire)| StreamFire {
                index,
                rule: fire.rule.as_str().into(),
                action: match fire.action {
                    RuleAction::Interrupt => StreamFireAction::Interrupt,
                    RuleAction::Remind => StreamFireAction::Reminder,
                    RuleAction::Report => StreamFireAction::Report,
                },
                text: fire.inject.clone(),
                judged: fire.judged,
            })
            .collect()
    }

    fn record_body(
        &self,
        fire: usize,
        at: jiff::Timestamp,
        entry: Option<EntryId>,
    ) -> Option<StreamWatchRecord> {
        let body = TtsrWatchAdapter::record_body(self, fire, at, entry);
        Some(StreamWatchRecord {
            kind: "rule_fired".into(),
            body: RawJson::parse(&body).ok()?,
        })
    }

    fn gate_record(&mut self, fire: usize, entry: Option<EntryId>) {
        TtsrWatchAdapter::gate_record(self, fire, entry);
    }
}
