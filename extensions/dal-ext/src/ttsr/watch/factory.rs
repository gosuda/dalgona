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
//! with the loop is needed. Judged matches surface a stopping candidate after
//! the provider stream is dropped; the bool verdict then decides whether the
//! retry is delivered.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dal_agent::ext::{
    BoxFuture, HookError, StreamFire, StreamFireAction, StreamWatch, StreamWatchRecord, TurnInfo,
    WatchFactory,
};
use dal_core::{
    Channel, EntryId, ModelRoute, Notice, RawJson, RulesConfig, SessionId, StreamVerdict, TurnId,
};
use tokio_util::sync::CancellationToken;

use super::super::build::RuleSet;
use super::super::gate::{Gate, resolve_cfg};
use super::super::judged::{JudgedLane, MatchDecision};
use super::super::lane::{LANE_RULE, Lanes, lane_rule};
use super::super::readers::EditStyle;
use super::super::texts::{RuleSubject, judge_failure_note, judge_pass_note};
use super::super::value::RuleAction;
use super::{Fire, Watch, WatchBudget, WatchError, create};

/// The turn a budget count belongs to, with its delivered interrupts.
#[derive(Debug)]
struct TurnBudget {
    turn: Option<TurnId>,
    interrupts: u32,
}

/// The model-dependent patch reader selected by product composition.
pub type EditStylePicker = Arc<dyn Fn(&ModelRoute) -> EditStyle + Send + Sync>;

/// Builds one [`TtsrWatchAdapter`] per model request from a shared rule
/// snapshot and repeat gate.
pub struct TtsrWatchFactory {
    set: Arc<RuleSet>,
    gate: Arc<Mutex<Gate>>,
    cfg: RulesConfig,
    ws_root: PathBuf,
    pick_edit_style: EditStylePicker,
    judge: Option<crate::judge::Judge>,
    budget: Arc<Mutex<TurnBudget>>,
    judged: Arc<Mutex<JudgedLane>>,
    /// The session-keyed lane store, present only when the product composes
    /// the repetitive-turns lane.
    lanes: Option<Arc<Lanes>>,
}

impl TtsrWatchFactory {
    /// Shares the retained rules, durable gate, judge, and turn budget.
    pub fn new(
        set: Arc<RuleSet>,
        gate: Arc<Mutex<Gate>>,
        cfg: RulesConfig,
        ws_root: PathBuf,
        pick_edit_style: EditStylePicker,
        judge: Option<crate::judge::Judge>,
    ) -> Self {
        Self {
            set,
            gate,
            cfg,
            ws_root,
            pick_edit_style,
            judge,
            budget: Arc::new(Mutex::new(TurnBudget {
                turn: None,
                interrupts: 0,
            })),
            judged: Arc::new(Mutex::new(JudgedLane::new())),
            lanes: None,
        }
    }

    /// Composes the repetitive-turns lane: each request's watch delivers the
    /// session's parked lane fire, if any, before its first feed.
    #[must_use]
    pub fn with_lane(mut self, lanes: Arc<Lanes>) -> Self {
        self.lanes = Some(lanes);
        self
    }
}

impl WatchFactory for TtsrWatchFactory {
    fn start(&self, turn: &TurnInfo<'_>) -> Option<Box<dyn StreamWatch>> {
        let watch_budget = {
            let mut budget = self.budget.lock().ok()?;
            // The budget follows real turn ordering, not watch-creation
            // order: a stale start for an older turn leaves the live turn's
            // interrupt count alone, so a late-created watcher can neither
            // resurrect an exhausted budget nor spend the live turn's.
            if budget.turn.is_none_or(|current| turn.turn > current) {
                budget.turn = Some(turn.turn);
                budget.interrupts = 0;
            }
            if budget.interrupts < self.cfg.max_retries {
                WatchBudget::Interrupts
            } else {
                WatchBudget::RemindersOnly
            }
        };
        let mut gate = self.gate.lock().ok()?;
        if let Some(seeds) = turn.gate_seeds {
            gate.restore_visible(seeds, turn.turn);
        }
        let edit_style = turn
            .model
            .map_or(EditStyle::Replace, |route| (self.pick_edit_style)(route));
        let mut watch = create(
            &self.set,
            &gate,
            turn.turn,
            watch_budget,
            &self.cfg,
            &self.ws_root,
            edit_style,
        );
        // The parked lane fire rides this request's watch as fire index 0,
        // ahead of any feed, and stays parked until its reminder record is
        // durable: a cancelled or shed batch resurfaces it on the next
        // request.
        if let Some(lanes) = &self.lanes
            && let Some(excerpt) = lanes.pending(turn.session)
        {
            let repeat = resolve_cfg(lane_rule(), &self.cfg);
            if gate.eligible(lane_rule(), turn.turn, &repeat) {
                watch.push_lane_fire(&excerpt);
            }
        }
        Some(Box::new(TtsrWatchAdapter {
            watch,
            gate: Arc::clone(&self.gate),
            turn: turn.turn,
            max_retries: self.cfg.max_retries,
            budget: Arc::clone(&self.budget),
            judge: self.judge.clone(),
            judged: Arc::clone(&self.judged),
            lanes: self
                .lanes
                .as_ref()
                .map(|lanes| (Arc::clone(lanes), turn.session)),
            stopped: false,
            reported_fires: 0,
            notices: Vec::new(),
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
    judge: Option<crate::judge::Judge>,
    judged: Arc<Mutex<JudgedLane>>,
    /// The lane store and this session's id, present only when the product
    /// composes the repetitive-turns lane.
    lanes: Option<(Arc<Lanes>, SessionId)>,
    stopped: bool,
    reported_fires: usize,
    notices: Vec<Notice>,
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
    ) -> Option<Box<str>> {
        self.watch.record_body(fire, at, entry)
    }

    /// Latches one delivered fire in the shared repeat gate; see
    /// [`Watch::gate_record`]. A poisoned gate skips the latch. A durable
    /// lane reminder also clears the session's parked fire, so it is not
    /// resurfaced; a batch that never becomes durable skips this call and
    /// the fire rides the next request instead.
    pub fn gate_record(&self, fire: usize, entry: Option<EntryId>) {
        if let Ok(mut gate) = self.gate.lock() {
            self.watch.gate_record(&mut gate, fire, entry);
        }
        if let Some((lanes, session)) = &self.lanes
            && entry.is_some()
            && self
                .watch
                .fires()
                .get(fire)
                .is_some_and(|fire| fire.rule.as_str() == LANE_RULE)
        {
            lanes.delivered(*session);
        }
    }

    /// Takes notices produced while resolving judged fires.
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    fn interrupt_verdict(&mut self, before: usize) -> StreamVerdict {
        let mut candidate = None;
        for index in before..self.watch.fires().len() {
            if self.watch.fires()[index].action != RuleAction::Interrupt {
                continue;
            }
            if self.watch.fires()[index].judged {
                let decision = self.judged.lock().ok().map(|mut lane| {
                    lane.matched(
                        self.watch.fires()[index].rule.as_str(),
                        self.turn,
                        true,
                        self.max_retries,
                    )
                });
                if matches!(decision, Some(MatchDecision::JudgePending)) {
                    candidate = Some(index);
                    break;
                }
                self.watch.demote_judged(index);
                continue;
            }
            candidate = Some(index);
            break;
        }
        let Some(index) = candidate else {
            return StreamVerdict::Continue;
        };
        let (rule, inject) = {
            let fire = &self.watch.fires()[index];
            (Box::<str>::from(fire.rule.as_str()), fire.inject.clone())
        };
        let Some(inject) = inject else {
            return StreamVerdict::Continue;
        };
        let Ok(mut budget) = self.budget.lock() else {
            return StreamVerdict::Continue;
        };
        if budget.turn == Some(self.turn) && budget.interrupts < self.max_retries {
            budget.interrupts += 1;
            let interrupts = budget.interrupts;
            drop(budget);
            if let Some(note) = self.watch.notice_limit(interrupts) {
                self.notices.push(Notice {
                    turn: Some(self.turn),
                    kind: "rules".into(),
                    text: note,
                });
            }
            self.demote_after(index);
            return StreamVerdict::Interrupt { rule, inject };
        }
        drop(budget);
        self.demote_cap(index);
        StreamVerdict::Continue
    }

    fn demote_after(&mut self, selected: usize) {
        self.demote_range(selected.saturating_add(1));
    }

    fn demote_cap(&mut self, first: usize) {
        self.demote_range(first);
    }

    fn demote_range(&mut self, first: usize) {
        let end = self.watch.fires().len();
        let needs_reminder_budget = (first..end).any(|index| {
            let fire = &self.watch.fires()[index];
            fire.action == RuleAction::Interrupt && !fire.judged
        });
        if needs_reminder_budget {
            self.watch.set_budget(WatchBudget::RemindersOnly);
        }
        for index in first..end {
            if self.watch.fires()[index].action != RuleAction::Interrupt {
                continue;
            }
            if self.watch.fires()[index].judged {
                self.watch.demote_judged(index);
            } else {
                self.watch.demote_to_reminder(index);
            }
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
            Ok(super::WatchVerdict::Stop | super::WatchVerdict::Continue) => {
                self.stop_verdict(before)
            }
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
            Ok(super::WatchVerdict::Stop | super::WatchVerdict::Continue) => {
                self.stop_verdict(before)
            }
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

    fn resolve_judged(
        &mut self,
        fire: usize,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<bool, HookError>> {
        let Some((shared, prompt)) = self.watch.judge_input(fire) else {
            return Box::pin(std::future::ready(Ok(false)));
        };
        let Some(judge) = self.judge.clone() else {
            return Box::pin(std::future::ready(Ok(false)));
        };
        let Some(fired) = self.watch.fires().get(fire).cloned() else {
            return Box::pin(std::future::ready(Ok(false)));
        };
        let rule = fired.rule.to_string();
        let source = fired.source.clone();
        let path = fired.path.clone();
        let notices = &mut self.notices;
        let turn = self.turn;
        Box::pin(async move {
            let question = match crate::judge::JudgeQuestion::bool(&prompt) {
                Ok(question) => question,
                Err(error) => {
                    notices.push(judge_failure_notice(turn, &rule, &error.to_string()));
                    return Ok(false);
                }
            };
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(HookError::Cancelled),
                result = judge.judge("ttsr", &shared, question, Some(turn), None) => result,
            };
            match result {
                Ok(crate::judge::Verdict::Bool(verdict)) => {
                    if !verdict {
                        notices.push(Notice {
                            turn: Some(turn),
                            kind: "rules".into(),
                            text: judge_pass_note(&rule, render_subject(&source, path.as_deref()))
                                .into(),
                        });
                    }
                    Ok(verdict)
                }
                Ok(_) => {
                    notices.push(judge_failure_notice(
                        turn,
                        &rule,
                        "judge returned a non-boolean verdict",
                    ));
                    Ok(false)
                }
                Err(error) => {
                    notices.push(judge_failure_notice(turn, &rule, &error.to_string()));
                    Ok(false)
                }
            }
        })
    }

    fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    fn record_body(
        &self,
        fire: usize,
        at: jiff::Timestamp,
        entry: Option<EntryId>,
    ) -> Option<StreamWatchRecord> {
        let body = TtsrWatchAdapter::record_body(self, fire, at, entry)?;
        Some(StreamWatchRecord {
            kind: "rule_fired".into(),
            body: RawJson::parse(&body).ok()?,
        })
    }

    fn gate_record(&mut self, fire: usize, entry: Option<EntryId>) {
        TtsrWatchAdapter::gate_record(self, fire, entry);
    }
}

fn judge_failure_notice(turn: TurnId, rule: &str, cause: &str) -> Notice {
    Notice {
        turn: Some(turn),
        kind: "rules".into(),
        text: judge_failure_note(rule, cause).into(),
    }
}

fn render_subject<'a>(source: &'a super::SourceKind, path: Option<&'a str>) -> RuleSubject<'a> {
    match source {
        super::SourceKind::Text => RuleSubject::Reply,
        super::SourceKind::Thinking => RuleSubject::Reasoning,
        super::SourceKind::Tool { tool } => match path {
            Some(path) => RuleSubject::ToolCallOnPath { tool, path },
            None => RuleSubject::ToolCall { tool },
        },
    }
}
