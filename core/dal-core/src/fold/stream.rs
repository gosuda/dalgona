use crate::journal::{AssistantRoute, MAX_RECORD};
use crate::model::{Family, ModelRoute, ReplayCapture, ReplayRefusal};

use super::helpers::{
    CompletedResponse, InferredResponse, StreamEnd, assistant_stop, blocks_from_inference,
    compact_notice, compaction_started_notice, duplicate_call_failure, invalid, resolution_failure,
    truncated_args, zero_usage,
};
use super::types::Overflow;
use super::{
    Block, CallId, CompactionReason, Effect, Emit, Entry, EntryKind, InferFailure, PartialResponse,
    PendingCall, Phase, PlannedCall, Record, Rejection, ResolvedCall, Session, Stop, StreamEvent,
    TreeDelta, TurnEndStop, TurnId, TurnStage, UpdateKind, Usage, plan,
};
use crate::raw::RawJson;

/// The closing assistant entry of one completed response, ready to journal.
///
/// `kind` is the [`EntryKind::Assistant`] payload with its stop member
/// already spelled for the journal and the replay payload that rode the
/// encoded line (`None` after a refusal). `stop` keeps the stream's own
/// stop reason to drive the response's stop flow after journaling. `model`
/// is the route that produced the response: it also names the entry's
/// model and records the turn's last usage. `calls` are the response's
/// tool calls, still pending settlement. `failure` ends the turn Failed
/// instead of the stop flow, with the entry still journaled exactly as
/// streamed. Built once per response and consumed by journaling; nothing
/// is copied.
struct ClosingResponse {
    kind: EntryKind,
    model: ModelRoute,
    stop: Stop,
    calls: Vec<(CallId, Box<str>)>,
    failure: Option<Box<str>>,
}

/// Builds the closing entry's assistant kind: the entry-spelled stop member,
/// the model route's identifier, and the replay payload that rode the encoded
/// line (`None` after a refusal).
fn assistant_kind(
    api: Family,
    model: &ModelRoute,
    content: Vec<Block>,
    usage: Usage,
    stop: Stop,
    replay: Option<RawJson>,
) -> EntryKind {
    EntryKind::Assistant {
        api,
        model: model.id().into(),
        route: Some(AssistantRoute::from(model)),
        content,
        usage,
        stop: assistant_stop(stop),
        replay,
    }
}

impl Session {
    pub(super) fn stream(
        &mut self,
        turn: TurnId,
        event: StreamEvent,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return Ok(());
        }
        match event {
            StreamEvent::Delta { channel, text } => effects.push(Effect::Delta {
                turn,
                channel,
                text,
            }),
            StreamEvent::ToolCall { call, name, args } => {
                emit.updates.push(UpdateKind::ToolStarted {
                    call,
                    tool: name,
                    args,
                });
            }
            // MessageReplay is not visible output: the capture is never
            // emitted as a client text update. The fold reads it from the
            // completed inference at `response_completed`, so a live stream
            // event has nothing to do.
            StreamEvent::Usage(_)
            | StreamEvent::Stop(_)
            | StreamEvent::ThinkingReplay { .. }
            | StreamEvent::MessageReplay { .. } => {}
            StreamEvent::Compaction { .. } => {
                return Err(invalid(
                    "native compaction outcome appeared in a turn stream",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn stream_ended(
        &mut self,
        turn: TurnId,
        end: StreamEnd,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(&self.phase, Phase::Running { turn: active, stage: TurnStage::Streaming { .. }, .. } if *active == turn)
        {
            return Ok(());
        }
        let StreamEnd {
            model,
            family,
            result,
            partial,
        } = end;
        self.active_model = Some(model.clone());
        self.active_family = Some(family);
        match result {
            Ok(inference) => {
                let response = CompletedResponse {
                    model,
                    family,
                    inference,
                };
                self.response_completed(turn, response, now, emit, effects)
            }
            Err(failure) => self.stream_failed(turn, failure, partial, now, emit, effects),
        }
    }

    pub(super) fn stream_failed(
        &mut self,
        turn: TurnId,
        failure: InferFailure,
        partial: Option<PartialResponse>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let (message, overflow): (Box<str>, bool) = match failure {
            InferFailure::Retryable { .. } => return Ok(()),
            InferFailure::Cancelled => {
                return self.end_turn(turn, TurnEndStop::Cancelled, partial, now, emit, effects);
            }
            InferFailure::Overflow { message, .. } => {
                if self.turn_flags.overflow == Overflow::Clear
                    && self.overflow_compaction(turn, emit, effects)
                {
                    return Ok(());
                }
                (
                    format!("Context overflow recovery failed: {message}").into_boxed_str(),
                    true,
                )
            }
            InferFailure::Fatal { message, .. } => (message, false),
            error @ (InferFailure::SyntheticCycle { .. } | InferFailure::SyntheticDepth { .. }) => {
                (error.to_string().into_boxed_str(), false)
            }
        };
        if overflow {
            self.turn_flags.overflow = Overflow::Unrecovered;
        }
        self.end_turn(
            turn,
            TurnEndStop::Failed { message },
            partial,
            now,
            emit,
            effects,
        )
    }

    /// Formats the refusal reason and, when available, the measured line length.
    fn refusal_message(reason: ReplayRefusal, measured: Option<u64>) -> Box<str> {
        let token = reason.token();
        match measured {
            Some(len) if reason == ReplayRefusal::LineBound => format!(
                "The provider replay capture for this response could not be stored ({token}): its journal line is {len} bytes, over the {MAX_RECORD} record bound, so the response was kept without it.",
            )
            .into(),
            _ if reason == ReplayRefusal::LineBound => format!(
                "The provider replay capture for this response could not be stored ({token}): its journal line is over the {MAX_RECORD} record bound, so the response was kept without it.",
            )
            .into(),
            _ => format!(
                "The provider replay capture for this response could not be retained ({token}), so the turn ended without it.",
            )
            .into(),
        }
    }

    /// Starts overflow compaction; returns `false` when no compactor can run.
    pub(super) fn overflow_compaction(
        &mut self,
        turn: TurnId,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> bool {
        let Some(limits) = self
            .limits
            .filter(|limits| limits.compact.compactor_available)
        else {
            if !self.compaction_none_notified {
                emit.updates
                    .push(compact_notice(Some(turn), "compact.none"));
                self.compaction_none_notified = true;
            }
            return false;
        };
        self.turn_flags.pending_reminders.clear();
        self.turn_flags.overflow = Overflow::Compacted;
        self.pending_compaction = Some(CompactionReason::Overflow);
        self.phase = Phase::Running {
            turn,
            round: self.current_round(),
            stage: TurnStage::Compacting {
                reason: CompactionReason::Overflow,
                automatic: true,
            },
        };
        emit.updates.push(compaction_started_notice(
            Some(turn),
            CompactionReason::Overflow,
            self.measured_context_tokens(),
            limits.window,
        ));
        effects.push(Effect::Compact {
            turn: Some(turn),
            first_kept: self.compaction_cut_point(),
        });
        true
    }

    pub(super) fn response_completed(
        &mut self,
        turn: TurnId,
        response: CompletedResponse,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        self.turn_flags.overflow = Overflow::Clear;
        let CompletedResponse {
            model: response_model,
            family: response_family,
            inference,
        } = response;
        let InferredResponse {
            blocks,
            calls,
            usage,
            stop,
            replay,
        } = blocks_from_inference(inference)?;
        let Some(stop) = stop else {
            let partial = Some(PartialResponse {
                content: blocks,
                usage: usage.unwrap_or(zero_usage()),
            });
            let message = "provider inference ended without a stop event".into();
            return self.end_turn(
                turn,
                TurnEndStop::Failed { message },
                partial,
                now,
                emit,
                effects,
            );
        };
        let usage = usage.unwrap_or(zero_usage());
        // Exact-line sizing: peek the entry id without allocating it, build
        // the candidate entry with the payload aboard, and encode the real
        // Record::Assistant - id, parent, timestamp, payload, line feed -
        // through the journal's own encoder. The line must fit the store's
        // record bound exactly; one byte over refuses the capture.
        let (replay, refusal) = match replay {
            None => (None, None),
            Some(ReplayCapture::Refused { reason }) => (None, Some((reason, None))),
            Some(ReplayCapture::Captured { payload }) => {
                let Some(id) = self.next_entry else {
                    return Err(invalid("entry id space exhausted"));
                };
                let entry = Entry {
                    id,
                    parent: self.tree.leaf,
                    at: now,
                    kind: assistant_kind(
                        response_family,
                        &response_model,
                        blocks.clone(),
                        usage,
                        stop,
                        Some(payload.clone()),
                    ),
                };
                let line = crate::journal::encode(&Record::Assistant(entry))
                    .map_err(|error| invalid(&error.to_string()))?;
                let line_len =
                    u64::try_from(line.len()).map_err(|_| invalid("record length overflow"))?;
                if line_len > MAX_RECORD {
                    (None, Some((ReplayRefusal::LineBound, Some(line_len))))
                } else {
                    (Some(payload), None)
                }
            }
        };
        let closing = ClosingResponse {
            kind: assistant_kind(
                response_family,
                &response_model,
                blocks,
                usage,
                stop,
                replay,
            ),
            model: response_model,
            stop,
            calls,
            failure: refusal.map(|(reason, measured)| Self::refusal_message(reason, measured)),
        };
        self.complete_entry(turn, closing, now, emit, effects)
    }

    /// Journals the response content, then ends a failed turn or handles its stop.
    fn complete_entry(
        &mut self,
        turn: TurnId,
        closing: ClosingResponse,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let ClosingResponse {
            kind,
            model,
            stop,
            calls,
            failure,
        } = closing;
        let EntryKind::Assistant { usage, .. } = &kind else {
            return Err(invalid("closing assistant entry has a foreign kind"));
        };
        let usage = *usage;
        let assistant = self.entry(now, kind)?;
        self.last_usage = Some((model, usage.input_tokens, self.compactions));
        let view = self.tree.append(assistant.clone());
        emit.records.push(Record::Assistant(assistant));
        self.turn_totals.add_usage(usage)?;
        let mut pending: Vec<PendingCall> = Vec::new();
        for (call, name) in calls {
            if pending.iter().all(|item| item.call != call) {
                pending.push(PendingCall {
                    call,
                    name,
                    started: false,
                    promotes: None,
                });
            }
        }
        emit.updates.push(UpdateKind::Tree(TreeDelta {
            added: vec![view],
            leaf: self.tree.leaf,
        }));
        emit.updates.push(UpdateKind::Usage(crate::view::UsageView {
            usage,
            context_tokens: self.tokens_since_last_compaction(),
            context_window: self.limits.map_or(0, |limits| limits.window),
        }));
        match failure {
            // The refused capture is the turn's failure: the closing content
            // stays journaled, and the typed reason travels in the TurnEnd
            // record and the turn.failed notice. Calls are empty by
            // construction - a capture on a tool-call response is dropped in
            // the stream-event policy - so there is nothing to settle.
            Some(message) => self.end_turn(
                turn,
                TurnEndStop::Failed { message },
                None,
                now,
                emit,
                effects,
            ),
            None => self.response_stopped(turn, stop, pending, now, emit, effects),
        }
    }

    pub(super) fn response_stopped(
        &mut self,
        turn: TurnId,
        stop: Stop,
        pending: Vec<PendingCall>,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        if !matches!(stop, Stop::Cancelled) {
            self.drain_pending_reminders(turn, now, emit)?;
        }
        let round = self.current_round();
        let end = match stop {
            Stop::Length if !pending.is_empty() => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                return self.boundary(turn, now, true, emit, effects);
            }
            Stop::Filter => {
                self.fail_calls(&pending, truncated_args, now, emit)?;
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Boundary,
                };
                TurnEndStop::Filter
            }
            Stop::Length => TurnEndStop::Length,
            Stop::EndTurn => {
                let stage = if pending.is_empty() {
                    self.turn_flags.end_after_boundary = true;
                    TurnStage::Boundary
                } else {
                    TurnStage::Resolving { pending }
                };
                self.phase = Phase::Running { turn, round, stage };
                return Ok(());
            }
            Stop::Cancelled | Stop::Failed | Stop::MaxSteps => {
                self.phase = Phase::Running {
                    turn,
                    round,
                    stage: TurnStage::Resolving { pending },
                };
                match stop {
                    Stop::Cancelled => TurnEndStop::Cancelled,
                    Stop::MaxSteps => TurnEndStop::MaxSteps,
                    _ => TurnEndStop::Failed {
                        message: "provider ended with a failed stop".into(),
                    },
                }
            }
        };
        self.end_turn(turn, end, None, now, emit, effects)
    }

    pub(super) fn resolved(
        &mut self,
        turn: TurnId,
        calls: &[ResolvedCall],
        answerer_attached: bool,
        now: jiff::Timestamp,
        emit: &mut Emit,
        effects: &mut Vec<Effect>,
    ) -> Result<(), Rejection> {
        let expected = match &self.phase {
            Phase::Running {
                turn: active,
                stage: TurnStage::Resolving { pending },
                ..
            } if *active == turn => pending.clone(),
            _ => return Ok(()),
        };
        if calls
            .iter()
            .any(|call| !expected.iter().any(|item| item.call == call.call))
            || expected
                .iter()
                .any(|item| !calls.iter().any(|call| call.call == item.call))
        {
            return Err(invalid(
                "resolved calls do not match the pending response calls",
            ));
        }
        let mut planned = Vec::new();
        let mut promotes = Vec::new();
        for item in &expected {
            let mut matching = calls.iter().filter(|call| call.call == item.call);
            let Some(call) = matching.next() else {
                continue;
            };
            if matching.next().is_some() {
                let text = duplicate_call_failure(call);
                self.result_entry(item, text, true, None, now, emit)?;
                continue;
            }
            if let Some(text) = resolution_failure(call) {
                self.result_entry(item, text, true, None, now, emit)?;
                continue;
            }
            let Ok(class) = &call.result else {
                continue;
            };
            planned.push(PlannedCall {
                call: item.call.clone(),
                name: call.name.clone(),
                class: class.clone(),
            });
            promotes.push(call.promoted);
        }
        let units = plan(&planned, &self.policy(answerer_attached));
        let pending = planned
            .into_iter()
            .zip(promotes)
            .map(|(call, promoted)| PendingCall {
                call: call.call,
                name: call.name.to_string().into(),
                started: false,
                promotes: promoted.then_some(call.name),
            })
            .collect::<Vec<_>>();
        let round = self.current_round();
        if pending.is_empty() {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Boundary,
            };
            self.boundary(turn, now, true, emit, effects)?;
        } else {
            self.phase = Phase::Running {
                turn,
                round,
                stage: TurnStage::Dispatching { pending },
            };
            effects.push(Effect::Dispatch { turn, units });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        AssistantStop, ClientId, Command, Event, Expect, HookEvent, HookOutcome, HookVerdict, Part,
        Phase, StreamChannel,
    };
    use super::*;
    use crate::model::Inference;
    use crate::model::ReplayCapture;
    use crate::raw::RawJson;
    use crate::update::Notice;

    const STAMP: jiff::Timestamp = jiff::Timestamp::UNIX_EPOCH;

    fn session() -> Session {
        Session::replay([], STAMP).unwrap().0
    }

    fn client() -> ClientId {
        ClientId::new("test")
    }

    fn usage(tokens: u64) -> Usage {
        Usage {
            input_tokens: tokens,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        }
    }

    fn route() -> ModelRoute {
        ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
        }
    }

    fn prompt(text: &str) -> Event {
        Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            },
            by: client(),
        }
    }

    fn send(session: &mut Session, event: Event) -> Vec<Effect> {
        let mut out = Vec::new();
        session.step(event, STAMP, &mut out).unwrap();
        out
    }

    fn begin(session: &mut Session) -> TurnId {
        send(session, prompt("question"));
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        send(
            session,
            Event::Guard {
                turn,
                call: None,
                extension: None,
                outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
                    .unwrap(),
            },
        );
        turn
    }

    fn stream_end(session: &mut Session, turn: TurnId, events: Vec<StreamEvent>) -> Vec<Effect> {
        send(
            session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(Inference { events }),
                partial: None,
            },
        )
    }

    fn assistant_entry(out: &[Effect]) -> Entry {
        for effect in out {
            if let Effect::Emit(emit) = effect {
                for record in &emit.records {
                    if let Record::Assistant(entry) = record {
                        return entry.clone();
                    }
                }
            }
        }
        panic!("no assistant entry in effects");
    }

    fn turn_end(out: &[Effect]) -> &TurnEndStop {
        for effect in out {
            if let Effect::Emit(emit) = effect {
                for record in &emit.records {
                    if let Record::TurnEnd { stop, .. } = record {
                        return stop;
                    }
                }
            }
        }
        panic!("no TurnEnd record in effects");
    }

    fn failed_message(stop: &TurnEndStop) -> &str {
        match stop {
            TurnEndStop::Failed { message } => message.as_ref(),
            other => panic!("expected Failed stop, got {other:?}"),
        }
    }

    fn capture(payload: &str) -> StreamEvent {
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Captured {
                payload: RawJson::parse(payload).unwrap(),
            },
        }
    }

    #[test]
    fn a_captured_payload_journals_on_the_assistant_entry() {
        let mut session = session();
        let turn = begin(&mut session);
        let out = stream_end(
            &mut session,
            turn,
            vec![
                StreamEvent::Delta {
                    channel: StreamChannel::Text,
                    text: "Bonjour.".into(),
                },
                capture(r#"{"records":["r1"]}"#),
                StreamEvent::Usage(usage(10)),
                StreamEvent::Stop(Stop::EndTurn),
            ],
        );
        let entry = assistant_entry(&out);
        let EntryKind::Assistant {
            replay, content, ..
        } = &entry.kind
        else {
            panic!("expected an assistant entry");
        };
        let Some(payload) = replay else {
            panic!("the captured payload must journal on the assistant entry");
        };
        assert_eq!(payload.as_str(), r#"{"records":["r1"]}"#);
        assert_eq!(
            content.as_slice(),
            [Block::Text {
                text: "Bonjour.".into(),
                replay: None,
            }]
        );
    }

    #[test]
    fn a_capture_on_a_tool_call_response_is_dropped_silently() {
        let mut session = session();
        let turn = begin(&mut session);
        let out = stream_end(
            &mut session,
            turn,
            vec![
                StreamEvent::ToolCall {
                    call: CallId::new("call-1"),
                    name: "read_file".into(),
                    args: RawJson::parse("{}").unwrap(),
                },
                capture(r#"{"records":[]}"#),
                StreamEvent::Usage(usage(5)),
                StreamEvent::Stop(Stop::EndTurn),
            ],
        );
        let entry = assistant_entry(&out);
        let EntryKind::Assistant { replay, .. } = &entry.kind else {
            panic!("expected an assistant entry");
        };
        assert!(
            replay.is_none(),
            "a tool-call response is not a message replay"
        );
    }

    #[test]
    fn a_line_one_byte_over_the_bound_refuses_with_line_bound() {
        // La décision de borne elle-même (ligne exacte acceptée, +1
        // refusée) exige un vrai payload ~64 MiB encodé par le codec : elle
        // est pilotée par la preuve parent (item 1 de la matrice), pas par
        // ce module. Ici : le message LineBound nomme le token, la
        // longueur mesurée et la limite exacte.
        let message = Session::refusal_message(ReplayRefusal::LineBound, Some(MAX_RECORD + 1));
        assert!(message.contains("line_bound"), "{message}");
        assert!(message.contains("67108865"), "{message}");
        assert!(message.contains(&MAX_RECORD.to_string()), "{message}");
    }

    #[test]
    fn a_refused_capture_journals_the_content_and_fails_the_turn() {
        let mut session = session();
        let turn = begin(&mut session);
        let out = stream_end(
            &mut session,
            turn,
            vec![
                StreamEvent::Delta {
                    channel: StreamChannel::Text,
                    text: "Texte deja diffuse.".into(),
                },
                StreamEvent::MessageReplay {
                    capture: ReplayCapture::Refused {
                        reason: ReplayRefusal::NoRecords,
                    },
                },
                StreamEvent::Usage(usage(10)),
                StreamEvent::Stop(Stop::EndTurn),
            ],
        );
        // L'entry journals le contenu exactement diffuse, sans payload.
        let entry = assistant_entry(&out);
        let EntryKind::Assistant {
            replay,
            content,
            stop,
            ..
        } = &entry.kind
        else {
            panic!("expected an assistant entry");
        };
        assert!(replay.is_none(), "a refused capture journals no payload");
        assert_eq!(
            content.as_slice(),
            [Block::Text {
                text: "Texte deja diffuse.".into(),
                replay: None,
            }]
        );
        assert_eq!(*stop, AssistantStop::Done);
        // Le tour se termine Failed avec le token typé dans le message,
        // pas par un stream Err qui aurait perdu le texte.
        let message = failed_message(turn_end(&out));
        assert!(message.contains("no_records"), "{message}");
        // Exactement un Record::TurnEnd : le refus n'end the turn twice.
        let turn_ends = out
            .iter()
            .filter_map(|effect| match effect {
                Effect::Emit(emit) => Some(emit),
                _ => None,
            })
            .flat_map(|emit| emit.records.iter())
            .filter(|record| matches!(record, Record::TurnEnd { .. }))
            .count();
        assert_eq!(turn_ends, 1, "the refusal ends the turn exactly once");
    }

    #[test]
    fn a_live_message_replay_event_is_never_client_output() {
        let mut session = session();
        let turn = begin(&mut session);
        let out = send(
            &mut session,
            Event::Stream {
                turn,
                event: capture(r#"{"records":["r1"]}"#),
            },
        );
        assert!(
            !out.iter().any(|effect| matches!(
                effect,
                Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(
                    update,
                    UpdateKind::Delta { .. } | UpdateKind::Notice(Notice { .. })
                ))
            )),
            "MessageReplay is not visible output"
        );
    }
}
