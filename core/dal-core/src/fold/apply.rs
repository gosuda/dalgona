use super::helpers::{HookTarget, StreamEnd, invalid};
use super::round::Settlement;
use super::types::{QuestionRef, QueuedInput};
use super::{
    Answer, ClientId, CompactLimits, Effect, Emit, Event, Family, JobId, JobKind, JobOutcome,
    Limits, ModelRoute, Name, Part, Phase, Question, Record, Rejection, Reply, Request, RequestId,
    Session, TurnId, UpdateKind,
};
use crate::update::ResolutionCause;

impl Session {
    pub(super) fn apply(
        &mut self,
        event: Event,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        match event {
            Event::Command { cmd, by } => return self.command(cmd, by, now, emit, effects),
            Event::Guard {
                turn,
                call,
                extension,
                outcome,
            } => {
                let target = HookTarget { call, extension };
                return self.guard(turn, target, outcome, now, emit, effects);
            }
            Event::StreamVerdict { turn, verdict } => {
                return self.stream_verdict(turn, verdict, now, emit, effects);
            }
            Event::StreamRetry { turn, reminder } => {
                return self.stream_retry(turn, reminder, now, emit, effects);
            }
            Event::StreamReminder { turn, rule, text } => {
                self.stream_reminder(turn, rule, text);
            }
            Event::Inferred {
                at,
                who,
                purpose,
                usage,
            } => return self.inferred(at, who, purpose, usage, emit),
            Event::RequestOpened { request } => self.request_opened(&request),
            Event::GrantResolved {
                request,
                answer,
                by,
                was_default,
                cause,
            } => return self.grant_resolved(request, &answer, by, was_default, cause, now, emit),
            Event::JobStarted { job, kind } => self.job_started(job, kind, emit),
            Event::JobSettled { job, outcome } => self.job_settled(job, outcome, emit),
            Event::Wake {
                text,
                sources,
                jobs,
            } => return self.wake(text, sources, jobs, now, emit, effects),
            Event::Steer { turn: _, text } => self.queue_steer(text, effects),
            Event::Cancel { scope, partial } => {
                return self.cancel(scope, partial, now, emit, effects);
            }
            Event::Stream { turn, event } => return self.stream(turn, event, emit, effects),
            Event::RequestStarted {
                turn,
                model,
                family,
            } => self.request_started(turn, model, family),
            Event::StreamEnded {
                turn,
                model,
                family,
                result,
                partial,
            } => {
                let end = StreamEnd {
                    model,
                    family,
                    result,
                    partial,
                };
                return self.stream_ended(turn, end, now, emit, effects);
            }
            Event::Resolved {
                turn,
                calls,
                answerer_attached,
            } => return self.resolved(turn, &calls, answerer_attached, now, emit, effects),
            Event::CallStarted { turn, call } => self.call_started(turn, &call, now, emit),
            Event::Settled {
                turn,
                call,
                outcome,
                elapsed_ms,
            } => {
                let settlement = Settlement {
                    call,
                    outcome,
                    elapsed_ms,
                };
                return self.settled(turn, settlement, now, emit, effects);
            }
            Event::Boundary { turn } => return self.boundary(turn, now, true, emit, effects),
            Event::Limits {
                window,
                max_steps,
                compact,
            } => self.set_limits(window, max_steps, compact),
            Event::CompactionSettled { turn, outcome } => {
                return self.compaction_settled(turn, outcome, now, emit, effects);
            }
            Event::Close => self.close(),
        }
        Ok(())
    }

    fn inferred(
        &mut self,
        at: jiff::Timestamp,
        who: crate::Owner,
        purpose: crate::InferredPurpose,
        usage: super::Usage,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        if matches!(&self.phase, Phase::Running { .. }) {
            self.turn_totals.add_usage(usage)?;
        }
        emit.records.push(super::Record::Inferred {
            at,
            who,
            purpose,
            usage,
        });
        Ok(())
    }

    pub(super) fn request_opened(&mut self, request: &Request) {
        let tool = match &request.question {
            Question::Approval { tool, .. } => Some(tool.clone()),
            Question::Grant { .. }
            | Question::Select { .. }
            | Question::Confirm { .. }
            | Question::Text { .. } => None,
        };
        self.open_questions.push((
            request.id,
            QuestionRef {
                tool,
                turn: request.turn,
            },
        ));
    }

    pub(super) fn queue_steer(&mut self, text: Box<str>, effects: &mut Vec<Effect>) {
        self.queued_inputs
            .push(QueuedInput::Steer(vec![Part::Text { text }]));
        effects.push(Effect::Reply(Ok(Reply::Queued { turn: None })));
    }

    pub(super) fn request_started(&mut self, turn: TurnId, model: ModelRoute, family: Family) {
        if matches!(&self.phase, Phase::Running { turn: active, .. } if *active == turn) {
            self.active_model = Some(model);
            self.active_family = Some(family);
        }
    }

    pub(super) fn set_limits(&mut self, window: u64, max_steps: u32, compact: CompactLimits) {
        self.limits = Some(Limits {
            window,
            max_steps,
            compact,
        });
    }

    pub(super) fn close(&mut self) {
        self.phase = Phase::Closed;
        self.queued_inputs.clear();
    }

    /// Journals the broker's answer to a tracked question, once.
    ///
    /// The fold records `cause` as given and never infers it from the
    /// answer, the attribution, or the default flag. An unknown question
    /// steps to `Ok` with nothing emitted: its terminal record already
    /// exists, or it was never opened here. A session-wide allowance follows
    /// only from a real client answer, never from a default, a timeout, a
    /// cancellation, or a withdrawal.
    pub(super) fn grant_resolved(
        &mut self,
        request: RequestId,
        answer: &Answer,
        by: Option<ClientId>,
        was_default: bool,
        cause: ResolutionCause,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) -> Result<(), Rejection> {
        let Some(index) = self
            .open_questions
            .iter()
            .position(|(id, _)| *id == request)
        else {
            return Ok(());
        };
        let client_answered = cause == ResolutionCause::Answered && !was_default && by.is_some();
        let grant = if *answer == Answer::ApproveForSession && client_answered {
            self.open_questions[index]
                .1
                .tool
                .as_deref()
                .map(|tool| {
                    Name::parse_mapped_tool(tool)
                        .map_err(|_| invalid("approval tool name is invalid"))
                })
                .transpose()?
        } else {
            None
        };
        self.open_questions.remove(index);
        let by = by.unwrap_or_else(|| ClientId::new("core"));
        emit.records.push(Record::Resolved {
            at: now,
            request,
            answer: answer.clone(),
            by: by.clone(),
            was_default,
            cause: Some(cause),
        });
        if let Some(name) = grant {
            emit.records.push(Record::AllowAlways {
                at: now,
                tool: name.as_str().into(),
                by: by.clone(),
            });
            self.allow_always.insert(name);
        }
        emit.updates.push(UpdateKind::RequestResolved {
            id: request,
            answer: answer.clone(),
            by,
            cause: Some(cause),
        });
        Ok(())
    }

    /// Resolves the ending turn's still-open questions as core
    /// cancellations that name `cause`: a turn that ends leaves no request
    /// without its terminal record, so a withdrawal or expiry that lands after
    /// the close still finds that record already journaled instead of
    /// dropping it as unknown.
    ///
    /// The caller picks `Cancelled` when the turn ended cancelled and
    /// `Withdrawn` for every other end, where nobody answered and nobody
    /// cancelled the turn.
    ///
    /// Turnless questions keep waiting: the broker resolves only the ending
    /// turn's slots, so a service-grant question asked outside any turn must
    /// not gain a durable Cancel record while its broker waiter stays open.
    pub(super) fn cancel_open_questions(
        &mut self,
        turn: TurnId,
        cause: ResolutionCause,
        now: jiff::Timestamp,
        emit: &mut Emit,
    ) {
        let open = std::mem::take(&mut self.open_questions);
        for (request, question) in open {
            if question.turn != Some(turn) {
                self.open_questions.push((request, question));
                continue;
            }
            let by = ClientId::new("core");
            emit.records.push(Record::Resolved {
                at: now,
                request,
                answer: Answer::Cancel,
                by: by.clone(),
                was_default: false,
                cause: Some(cause),
            });
            emit.updates.push(UpdateKind::RequestResolved {
                id: request,
                answer: Answer::Cancel,
                by,
                cause: Some(cause),
            });
        }
    }

    /// Records a started job and tells clients once.
    ///
    /// A manual compaction is a phase of the session, not a job a client counts.
    pub(super) fn job_started(&mut self, job: JobId, kind: JobKind, emit: &mut Emit) {
        if self.live_jobs.iter().any(|(id, _)| *id == job) {
            return;
        }
        self.live_jobs.push((job, Some(kind)));
        if kind == JobKind::Compaction {
            if matches!(&self.phase, Phase::Compacting { job: None }) {
                self.phase = Phase::Compacting { job: Some(job) };
            }
            return;
        }
        emit.updates.push(UpdateKind::JobStarted { job });
    }

    /// Records a settled job and tells clients once.
    pub(super) fn job_settled(&mut self, job: JobId, outcome: JobOutcome, emit: &mut Emit) {
        let Some(index) = self.live_jobs.iter().position(|(id, _)| *id == job) else {
            return;
        };
        let (_, kind) = self.live_jobs.remove(index);
        if kind != Some(JobKind::Compaction) {
            self.ended_jobs.push((job, outcome));
            emit.updates.push(UpdateKind::JobSettled { job });
            return;
        }
        if matches!(&self.phase, Phase::Compacting { job: Some(active) } if *active == job) {
            self.manual_completion.job_settled = true;
            self.finish_manual_compaction_if_ready();
        }
    }
}
