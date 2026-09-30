use super::helpers::{
    HookTarget, compact_notice, compaction_started_notice, invalid, part_to_journal, zero_usage,
};
use super::types::{MAX_INTERRUPTS, ManualCompletion, QueuedInput};
use super::{
    CallId, CancelScope, ClientId, Command, CompactionReason, Effect, Emit, Entry, EntryId,
    EntryKind, HookOutcome, HookVerdict, JobId, Notice, Output, Part, Phase, Record, Rejection,
    Reply, Session, Step, StreamVerdict, ToolCallVerdict, TreeDelta, TurnCause, TurnId, TurnSource,
    TurnStage, UpdateKind,
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
                let turn = self.allocate_turn()?;
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
                effects.push(Effect::Reply(Ok(Reply::Queued)));
            }
            Command::FollowUp { turn: _, content } => {
                let turn = self.allocate_turn()?;
                self.queued_inputs.push(QueuedInput::FollowUp {
                    turn,
                    source: TurnSource::FollowUp { by, content },
                });
                effects.push(Effect::Reply(Ok(Reply::Queued)));
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
                self.tree.leaf = Some(id);
                self.projected_bytes = self.projected_bytes_on_branch();
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
        self.tree.append(entry.clone());
        emit.records.push(record(entry));
        emit.updates
            .push(UpdateKind::Settings(self.settings_view()));
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
        let turn = self.allocate_turn()?;
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
            HookVerdict::BeforeTurn(add) => self.begin_turn(turn, add, now, emit, effects),
            HookVerdict::BeforeRequest(Some(params)) if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) =>
            {
                self.request_params = params;
                Ok(())
            }
            HookVerdict::ToolCall(verdict) => {
                self.tool_call_guard(turn, target, verdict, now, emit, effects)
            }
            HookVerdict::BeforeRequest(_) | HookVerdict::Input(_) => Ok(()),
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
        let (mut content, cause) = match source {
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
        if let Some(text) = add
            && !text.is_empty()
        {
            let separator = if content
                .iter()
                .any(|part| matches!(part, Part::Text { text } if !text.is_empty()))
            {
                "\n\n"
            } else {
                ""
            };
            content.push(Part::Text {
                text: format!("{separator}{text}").into(),
            });
        }
        let entry_record = self.entry_at(
            entry,
            now,
            EntryKind::User {
                parts: content.iter().map(part_to_journal).collect(),
            },
        );
        let view = self.tree.append(entry_record.clone());
        emit.records.push(Record::TurnStart { at: now, turn });
        emit.records.push(Record::User(entry_record));
        emit.updates.push(UpdateKind::TurnStarted { turn, cause });
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        self.turn_flags.interrupts = 0;
        self.turn_flags.overflowed = false;
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
        self.result_entry(&item.call, &item.name, text, true, now, emit)?;
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

    pub(super) fn stream_reminder(
        &mut self,
        turn: TurnId,
        rule: &str,
        text: Box<str>,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        if !matches!(
            &self.phase,
            Phase::Running {
                turn: active,
                stage: TurnStage::Streaming { .. },
                ..
            } if *active == turn
        ) {
            return Ok(());
        }
        let entry = self.entry(
            now,
            EntryKind::Reminder {
                source: format!("rule:{rule}").into(),
                text,
            },
        )?;
        let view = self.tree.append(entry.clone());
        emit.records.push(Record::Reminder(entry));
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
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
            StreamVerdict::Interrupt { rule, inject }
                if self.turn_flags.interrupts < MAX_INTERRUPTS =>
            {
                let entry = self.entry(
                    now,
                    EntryKind::Reminder {
                        source: format!("rule:{rule}").into(),
                        text: inject,
                    },
                )?;
                self.turn_flags.interrupts += 1;
                let view = self.tree.append(entry.clone());
                emit.records.push(Record::Reminder(entry.clone()));
                emit.records.push(Record::RuleFired {
                    at: now,
                    turn,
                    rule: rule.clone(),
                    entry: entry.id,
                });
                emit.updates.push(UpdateKind::RuleFired { turn, rule });
                emit.updates.push(UpdateKind::Tree(TreeDelta {
                    added: vec![view],
                    leaf: self.tree.leaf,
                }));
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
