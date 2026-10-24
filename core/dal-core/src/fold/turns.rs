use crate::BEFORE_TURN_SOURCE;
use crate::ext::InputVerdict;

use super::helpers::{
    HookTarget, compact_notice, compaction_started_notice, invalid, part_to_journal, zero_usage,
};
use super::types::{ManualCompletion, Overflow, PendingReminder, QueuedInput};
use super::{
    CallId, CancelScope, ClientId, Command, CompactionReason, Effect, Emit, Entry, EntryId,
    EntryKind, GateSeed, HookOutcome, HookVerdict, JobId, Notice, Output, Part, Phase, Record,
    Rejection, Reply, Session, Step, StreamVerdict, ToolCallVerdict, TreeDelta, TurnCause, TurnId,
    TurnSource, TurnStage, UpdateKind,
};

impl Session {
    pub(super) fn command(
        &mut self,
        cmd: Command,
        by: ClientId,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match cmd {
            Command::Prompt { content, .. } => {
                let turn = self.allocate_turn(now, emit)?;
                let entry = self.allocate_entry_id()?;
                self.wake_run = 0;
                self.wake_attempt_turn = None;
                self.phase = Phase::Opening {
                    turn,
                    source: TurnSource::Prompt { by, content },
                    entry,
                };
                effects.push(Effect::Reply(Ok(Reply::Accepted {
                    turn,
                    message_id: entry,
                })));
            }
            Command::Steer { turn: _, content } => {
                self.queued_inputs.push(QueuedInput::Steer(content));
                effects.push(Effect::Reply(Ok(Reply::Queued { turn: None })));
            }
            Command::FollowUp { turn: _, content } => {
                let turn = self.allocate_turn(now, emit)?;
                self.queued_inputs.push(QueuedInput::FollowUp {
                    turn,
                    source: TurnSource::FollowUp { by, content },
                });
                effects.push(Effect::Reply(Ok(Reply::Queued { turn: Some(turn) })));
            }
            Command::CancelQueued { turn } => {
                return self.cancel_queued(turn, emit, effects);
            }
            Command::Cancel {
                scope: scope @ CancelScope::Job(_),
            } => {
                effects.push(Effect::Command {
                    cmd: Command::Cancel { scope },
                    by,
                });
            }
            Command::Cancel { scope } => return self.cancel(scope, None, now, emit, effects),
            Command::SetModel {
                model: route,
                save: _,
            } => {
                let kind = EntryKind::Model { route };
                return self.change_setting(kind, Record::Model, now, emit, effects);
            }
            Command::SetThinking { level, save: _ } => {
                let kind = EntryKind::Thinking { level };
                return self.change_setting(kind, Record::Thinking, now, emit, effects);
            }
            Command::SetApproval { mode, save: _ } => {
                let kind = EntryKind::Approval { mode };
                return self.change_setting(kind, Record::Approval, now, emit, effects);
            }
            Command::SetMode { mode, save: _ } => {
                let kind = EntryKind::Mode { mode };
                return self.change_setting(kind, Record::Mode, now, emit, effects);
            }
            Command::Rename(name) => {
                self.settings.name = Some(name.clone());
                emit.records.push(Record::Name {
                    at: now,
                    name: Some(name),
                });
                emit.updates
                    .push(UpdateKind::Settings(self.settings_view()));
                effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
            }
            Command::MoveLeaf(id) => {
                self.restore_branch_state(Some(id))
                    .map_err(|error| Rejection::Invalid {
                        reason: error.to_string().into(),
                    })?;
                emit.records.push(Record::Leaf {
                    at: now,
                    to: Some(id),
                });
                emit.updates.push(UpdateKind::Tree(TreeDelta {
                    added: Vec::new(),
                    leaf: Some(id),
                }));
                effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
            }
            Command::Compact { focus } => self.manual_compact(focus, emit, effects),
            command @ (Command::Fork(_)
            | Command::Clone
            | Command::Run { .. }
            | Command::SetScopedModels(_)
            | Command::Export { .. }
            | Command::ReloadPlugins) => {
                effects.push(Effect::Command { cmd: command, by });
            }
        }
        Ok(())
    }

    pub(super) fn change_setting(
        &mut self,
        kind: EntryKind,
        record: fn(Entry) -> Record,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let entry = self.entry(now, kind)?;
        self.replay_setting(&entry);
        let view = self.tree.append(entry.clone());
        emit.records.push(record(entry));
        emit.updates
            .push(UpdateKind::Settings(self.settings_view()));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
        Ok(())
    }

    pub(super) fn manual_compact(
        &mut self,
        focus: Option<Box<str>>,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) {
        let tokens = self.tokens_since_last_compaction();
        let limits = self.limits;
        let refusal = if self.compacted_at_leaf() {
            Some("compact.already")
        } else if !limits.is_some_and(|limits| limits.compact.compactor_available) {
            Some("compact.none")
        } else if tokens < limits.map_or(1, |limits| limits.compact.min_tokens) {
            Some("compact.nothing")
        } else {
            None
        };
        if let Some(key) = refusal {
            emit.updates.push(compact_notice(None, key));
            effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
            return;
        }
        self.pending_manual_focus = focus;
        self.pending_compaction = Some(CompactionReason::Manual);
        self.manual_completion = ManualCompletion::default();
        self.phase = Phase::Compacting { job: None };
        let window = limits.map_or(0, |limits| limits.window);
        emit.updates.push(compaction_started_notice(
            None,
            CompactionReason::Manual,
            self.measured_context_tokens(),
            window,
        ));
        effects.push(Effect::Compact {
            turn: None,
            first_kept: self.compaction_cut_point(),
        });
        effects.push(Effect::Reply(Ok(Reply::Done(Output::Nothing))));
    }

    pub(super) fn wake(
        &mut self,
        text: Box<str>,
        sources: Box<[Box<str>]>,
        jobs: Box<[JobId]>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        _effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if matches!(&self.phase, Phase::Compacting { .. }) {
            return Err(Rejection::BusyTurn);
        }
        let delivered = jobs.to_vec();
        let source = TurnSource::Wake {
            sources,
            jobs,
            content: vec![Part::Text { text }],
        };
        let turn = self.allocate_turn(now, emit)?;
        self.wake_run += 1;
        self.wake_attempt_turn = Some(turn);
        emit.records.push(Record::WakeAttempt {
            at: now,
            turn,
            count: self.wake_run,
            jobs: delivered.clone(),
        });
        self.delivered_jobs.extend(delivered);
        if matches!(&self.phase, Phase::Idle) {
            let entry = self.allocate_entry_id()?;
            self.phase = Phase::Opening {
                turn,
                source,
                entry,
            };
            return Ok(());
        }
        self.queued_inputs
            .push(QueuedInput::FollowUp { turn, source });
        Ok(())
    }

    pub(super) fn guard(
        &mut self,
        turn: TurnId,
        target: HookTarget,
        outcome: HookOutcome,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match outcome.into_verdict() {
            HookVerdict::Input(verdict) => self.input_guard(turn, verdict),
            HookVerdict::BeforeTurn(add) => self.begin_turn(turn, add, now, emit, effects),
            HookVerdict::BeforeRequest(Some(params)) if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) =>
            {
                self.request_params = params;
                Ok(())
            }
            HookVerdict::ToolCall(verdict) => {
                self.tool_call_guard(turn, target, verdict, now, emit, effects)
            }
            HookVerdict::BeforeRequest(_) => Ok(()),
        }
    }

    /// Applies the folded `input` verdict to the opening of `turn`.
    ///
    /// `Transform` replaces the content the opening carries, so the one
    /// `Record::User` that [`Self::begin_turn`] journals holds the final
    /// parts and the provider context derived from that entry matches the
    /// journal. `Continue` and `Handled` keep the admitted content. The
    /// verdict emits no record and no effect. A verdict for a turn whose
    /// opening is gone is stale and ignored, like any late guard.
    ///
    /// # Errors
    /// Rejects, leaving the opening unchanged, a transform that leaves no
    /// parts and a transform of a wake opening, which runs no input hooks.
    fn input_guard(&mut self, turn: TurnId, verdict: InputVerdict) -> Result<(), Rejection> {
        let InputVerdict::Transform(parts) = verdict else {
            return Ok(());
        };
        let Some(source) = self.opening_source_mut(turn) else {
            return Ok(());
        };
        if parts.is_empty() {
            return Err(invalid("input transform left the turn without content"));
        }
        match source {
            TurnSource::Prompt { content, .. } | TurnSource::FollowUp { content, .. } => {
                *content = parts;
                Ok(())
            }
            TurnSource::Wake { .. } => Err(invalid("wake openings take no input transform")),
        }
    }

    /// Borrows the input of the opening that awaits its `before_turn` verdict.
    fn opening_source_mut(&mut self, turn: TurnId) -> Option<&mut TurnSource> {
        match &mut self.phase {
            Phase::Opening {
                turn: active,
                source,
                ..
            } if *active == turn => Some(source),
            Phase::Settling {
                follow_up: Some((next, source)),
                ..
            } if *next == turn => Some(source),
            _ => None,
        }
    }

    /// Releases an opening whose hooks could not admit a durable turn.
    /// A stale opening completion cannot change another turn's phase.
    pub fn abandon_opening(&mut self, turn: TurnId) {
        let owned = match &self.phase {
            Phase::Opening { turn: active, .. } => *active == turn,
            Phase::Settling {
                follow_up: Some((next, _)),
                ..
            } => *next == turn,
            _ => false,
        };
        if owned {
            self.phase = Phase::Idle;
        }
    }

    /// Takes the input of the turn a `before_turn` verdict opens, reserving its entry id.
    pub(super) fn take_opening_input(
        &mut self,
        turn: TurnId,
    ) -> Result<Option<(TurnSource, EntryId)>, Rejection> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Opening {
                turn: active,
                source,
                entry,
            } if active == turn => Ok(Some((source, entry))),
            Phase::Settling {
                turn: active,
                follow_up: Some((next_turn, source)),
            } if next_turn == turn => match self.allocate_entry_id() {
                Ok(entry) => Ok(Some((source, entry))),
                Err(error) => {
                    self.phase = Phase::Settling {
                        turn: active,
                        follow_up: Some((next_turn, source)),
                    };
                    Err(error)
                }
            },
            other => {
                self.phase = other;
                Ok(None)
            }
        }
    }

    pub(super) fn begin_turn(
        &mut self,
        turn: TurnId,
        add: Option<Box<str>>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let Some((source, entry)) = self.take_opening_input(turn)? else {
            return Ok(());
        };
        self.turn_totals.reset();
        let (content, cause) = match source {
            TurnSource::Prompt { content, .. } => (content, TurnCause::User),
            TurnSource::Wake { content, .. } => (content, TurnCause::Wake),
            TurnSource::FollowUp { content, .. } => (content, TurnCause::FollowUp),
        };
        if cause != TurnCause::Wake
            && self
                .wake_attempt_turn
                .is_some_and(|attempt| attempt.get() < turn.get())
        {
            self.wake_run = 0;
            self.wake_attempt_turn = None;
        }
        let entry_record = self.entry_at(
            entry,
            now,
            EntryKind::User {
                parts: content.iter().map(part_to_journal).collect(),
            },
        );
        let mut added = vec![self.tree.append(entry_record.clone())];
        emit.records.push(Record::TurnStart { at: now, turn });
        emit.records.push(Record::User(entry_record));
        if let Some(text) = add.filter(|text| !text.is_empty()) {
            let kind = EntryKind::Reminder {
                source: BEFORE_TURN_SOURCE.into(),
                text,
            };
            let reminder = self.entry(now, kind)?;
            added.push(self.tree.append(reminder.clone()));
            emit.records.push(Record::Reminder(reminder));
        }
        emit.updates.push(UpdateKind::TurnStarted { turn, cause });
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added,
            leaf: self.tree.leaf,
        }));
        self.turn_flags.interrupts = 0;
        self.turn_flags.pending_reminders.clear();
        self.turn_flags.overflow = Overflow::Clear;
        self.turn_flags.suppressed_notice = false;
        self.argument_overrides.clear();
        self.phase = Phase::Running {
            turn,
            round: Step(0),
            stage: TurnStage::Boundary,
        };
        self.boundary(turn, now, false, emit, effects)
    }

    pub(super) fn tool_call_guard(
        &mut self,
        turn: TurnId,
        target: HookTarget,
        verdict: ToolCallVerdict,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match verdict {
            ToolCallVerdict::Allow => Ok(()),
            ToolCallVerdict::Rewrite { args } => {
                let call = target
                    .call
                    .ok_or_else(|| invalid("tool-call rewrite omitted its call id"))?;
                if let Some((_, previous)) = self
                    .argument_overrides
                    .iter_mut()
                    .find(|(id, _)| *id == call)
                {
                    *previous = args;
                } else {
                    self.argument_overrides.push((call, args));
                }
                Ok(())
            }
            ToolCallVerdict::Block { reason } => {
                let call = target
                    .call
                    .ok_or_else(|| invalid("tool-call block omitted its call id"))?;
                let extension = target
                    .extension
                    .ok_or_else(|| invalid("tool-call block omitted its extension name"))?;
                let text = format!("blocked by {extension}: {reason}").into();
                self.block_call(turn, &call, text, now, emit, effects)
            }
        }
    }

    pub(super) fn block_call(
        &mut self,
        turn: TurnId,
        call: &CallId,
        text: Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let (round, mut pending) = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Dispatching { pending },
            } if *active == turn => (*round, pending.clone()),
            _ => return Ok(()),
        };
        let Some(index) = pending.iter().position(|item| item.call == *call) else {
            return Ok(());
        };
        let item = pending.remove(index);
        self.result_entry(&item, text, true, None, now, emit)?;
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            return self.boundary(turn, now, true, emit, effects);
        }
        self.phase = Phase::Running {
            turn,
            round,
            stage: TurnStage::Dispatching { pending },
        };
        Ok(())
    }

    pub(super) fn stream_reminder(&mut self, turn: TurnId, rule: Box<str>, text: Box<str>) {
        if !matches!(
            &self.phase,
            Phase::Running {
                turn: active,
                stage: TurnStage::Streaming { .. },
                ..
            } if *active == turn
        ) {
            return;
        }
        self.turn_flags
            .pending_reminders
            .push(PendingReminder { rule, text });
    }

    fn deliver_rule_reminder(
        &mut self,
        turn: TurnId,
        rule: Box<str>,
        text: Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let entry = self.entry(
            now,
            EntryKind::Reminder {
                source: format!("rule:{rule}").into(),
                text,
            },
        )?;
        let seed = GateSeed {
            rule: rule.clone(),
            turn,
            entry: entry.id,
        };
        let view = self.tree.append(entry.clone());
        emit.records.push(Record::Reminder(entry));
        emit.records.push(Record::RuleFired {
            at: now,
            turn,
            rule: rule.clone(),
            entry: seed.entry,
        });
        emit.updates.push(UpdateKind::RuleFired { turn, rule });
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        self.rule_facts.push(seed);
        Ok(())
    }

    pub(super) fn drain_pending_reminders(
        &mut self,
        turn: TurnId,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let count = self.turn_flags.pending_reminders.len();
        if count == 0 {
            return Ok(());
        }
        let count = u64::try_from(count).map_err(|_| invalid("too many pending reminders"))?;
        if let Err(error) = self.entries_available(count) {
            self.turn_flags.pending_reminders.clear();
            return Err(error);
        }
        let pending = std::mem::take(&mut self.turn_flags.pending_reminders);
        for reminder in pending {
            self.deliver_rule_reminder(turn, reminder.rule, reminder.text, now, emit)?;
        }
        Ok(())
    }

    pub(super) fn stream_retry(
        &mut self,
        turn: TurnId,
        reminder: Option<(Box<str>, Box<str>)>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Streaming { .. },
            } if *active == turn => *round,
            _ => return Ok(()),
        };
        self.drain_pending_reminders(turn, now, emit)?;
        if let Some((rule, text)) = reminder {
            self.deliver_rule_reminder(turn, rule, text, now, emit)?;
        }
        self.phase = Phase::Running {
            turn,
            round,
            stage: TurnStage::Streaming {
                blocks: Vec::new(),
                usage: zero_usage(),
                calls: Vec::new(),
                suppressed_injects: Vec::new(),
            },
        };
        effects.push(Effect::Infer(self.request_plan(turn)));
        Ok(())
    }

    pub(super) fn stream_verdict(
        &mut self,
        turn: TurnId,
        verdict: StreamVerdict,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let round = match &self.phase {
            Phase::Running {
                turn: active,
                round,
                stage: TurnStage::Streaming { .. },
            } if *active == turn => *round,
            _ => return Ok(()),
        };
        match verdict {
            StreamVerdict::Continue => {}
            StreamVerdict::Interrupt { rule, inject } if self.can_interrupt_stream() => {
                self.drain_pending_reminders(turn, now, emit)?;
                self.deliver_rule_reminder(turn, rule, inject, now, emit)?;
                self.turn_flags.interrupts += 1;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Streaming {
                        blocks: Vec::new(),
                        usage: zero_usage(),
                        calls: Vec::new(),
                        suppressed_injects: Vec::new(),
                    },
                };
                effects.push(Effect::Infer(self.request_plan(turn)));
            }
            StreamVerdict::Interrupt { inject, .. } => {
                self.turn_flags.pending_suppressed.push(inject);
                let notify = !self.turn_flags.suppressed_notice;
                self.turn_flags.suppressed_notice = true;
                if notify {
                    emit.updates.push(UpdateKind::Notice(Notice {
                        turn: Some(turn),
                        kind: "rule.suppressed".into(),
                        text: "rule.suppressed".into(),
                    }));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Event, Expect, JournalPart};
    use super::*;

    fn at() -> jiff::Timestamp {
        jiff::Timestamp::UNIX_EPOCH
    }

    fn text(value: &str) -> Part {
        Part::Text { text: value.into() }
    }

    fn step(session: &mut Session, event: Event) -> Result<Vec<Effect>, Rejection> {
        let mut out = Vec::new();
        session.step(event, at(), &mut out)?;
        Ok(out)
    }

    fn guard(turn: TurnId, verdict: HookVerdict) -> Event {
        let outcome = HookOutcome::new(verdict.event(), verdict).unwrap();
        Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome,
        }
    }

    fn transform(turn: TurnId, parts: Vec<Part>) -> Event {
        guard(turn, HookVerdict::Input(InputVerdict::Transform(parts)))
    }

    fn open_turn(session: &Session) -> TurnId {
        match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("expected an opening, got {phase:?}"),
        }
    }

    fn prompt(session: &mut Session, content: Vec<Part>) -> TurnId {
        let command = Command::Prompt {
            expect: Expect::Idle,
            content,
        };
        let event = Event::Command {
            cmd: command,
            by: ClientId::new("test"),
        };
        step(session, event).unwrap();
        open_turn(session)
    }

    /// The parts of the one `User` record the batch journals.
    fn user_parts(effects: &[Effect]) -> Vec<JournalPart> {
        let batch = effects.iter().find_map(|effect| match effect {
            Effect::Emit(emit) => Some(&emit.records),
            _ => None,
        });
        let mut users = batch.unwrap().iter().filter_map(|record| match record {
            Record::User(entry) => Some(entry),
            _ => None,
        });
        let entry = users.next().unwrap();
        assert!(users.next().is_none(), "exactly one User record");
        match &entry.kind {
            EntryKind::User { parts } => parts.clone(),
            other => panic!("the User record holds {other:?}"),
        }
    }

    fn journaled(parts: &[Part]) -> Vec<JournalPart> {
        parts.iter().map(part_to_journal).collect()
    }

    fn opened(content: Vec<Part>) -> (Session, TurnId) {
        let (mut session, _) = Session::replay([], at()).unwrap();
        let turn = prompt(&mut session, content);
        (session, turn)
    }

    #[test]
    fn a_transform_replaces_the_content_the_user_record_journals() {
        let (mut session, turn) = opened(vec![text("original")]);
        let out = step(&mut session, transform(turn, vec![text("changed")])).unwrap();
        assert!(out.is_empty(), "the input guard emits nothing: {out:?}");
        let out = step(&mut session, guard(turn, HookVerdict::BeforeTurn(None))).unwrap();
        assert_eq!(user_parts(&out), journaled(&[text("changed")]));
    }

    #[test]
    fn continue_and_handled_keep_the_admitted_content() {
        for verdict in [InputVerdict::Continue, InputVerdict::Handled] {
            let (mut session, turn) = opened(vec![text("original")]);
            step(&mut session, guard(turn, HookVerdict::Input(verdict))).unwrap();
            let out = step(&mut session, guard(turn, HookVerdict::BeforeTurn(None))).unwrap();
            assert_eq!(user_parts(&out), journaled(&[text("original")]));
        }
    }

    #[test]
    fn a_transform_for_a_closed_opening_is_ignored() {
        let (mut session, turn) = opened(vec![text("original")]);
        let cancel = Event::Command {
            cmd: Command::Cancel {
                scope: CancelScope::Turn(turn),
            },
            by: ClientId::new("test"),
        };
        step(&mut session, cancel).unwrap();
        assert_eq!(session.phase(), &Phase::Idle);
        step(&mut session, transform(turn, vec![text("late")])).unwrap();
        assert_eq!(session.phase(), &Phase::Idle);
    }

    #[test]
    fn an_empty_transform_is_refused_and_leaves_the_content() {
        let (mut session, turn) = opened(vec![text("original")]);
        let refused = step(&mut session, transform(turn, Vec::new()));
        assert!(matches!(refused, Err(Rejection::Invalid { .. })));
        let out = step(&mut session, guard(turn, HookVerdict::BeforeTurn(None))).unwrap();
        assert_eq!(user_parts(&out), journaled(&[text("original")]));
    }

    #[test]
    fn a_wake_opening_refuses_a_transform() {
        let (mut session, _) = Session::replay([], at()).unwrap();
        let wake = Event::Wake {
            text: "wake".into(),
            sources: Box::default(),
            jobs: Box::default(),
        };
        step(&mut session, wake).unwrap();
        let turn = open_turn(&session);
        let refused = step(&mut session, transform(turn, vec![text("changed")]));
        assert!(matches!(refused, Err(Rejection::Invalid { .. })));
        let out = step(&mut session, guard(turn, HookVerdict::BeforeTurn(None))).unwrap();
        assert_eq!(user_parts(&out), journaled(&[text("wake")]));
    }
}
