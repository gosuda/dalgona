use super::helpers::{part_to_journal, truncated_args};
use super::types::MAX_STEERS;

use super::*;
use std::{num::NonZeroU64, path::PathBuf};

fn id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).unwrap())
}
fn entry(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).unwrap())
}
fn stamp() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}
fn session() -> Session {
    Session::replay([], stamp()).unwrap().0
}
fn client() -> ClientId {
    ClientId::new("test")
}
fn name(value: &str) -> Name {
    Name::parse(value).unwrap()
}
fn route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "test-model".into(),
    }
}
fn usage(tokens: u64) -> Usage {
    Usage {
        input_tokens: tokens,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
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
fn guard(turn: TurnId) -> Event {
    Event::Guard {
        turn,
        call: None,
        extension: None,
        outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None)).unwrap(),
    }
}
fn send(session: &mut Session, event: Event) -> Result<Vec<Effect>, Rejection> {
    let mut out = Vec::new();
    session.step(event, stamp(), &mut out)?;
    Ok(out)
}
fn begin(session: &mut Session) -> TurnId {
    send(session, prompt("question")).unwrap();
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    send(session, guard(turn)).unwrap();
    turn
}
fn limits(min_tokens: u64) -> Event {
    Event::Limits {
        window: 100_000,
        max_steps: 0,
        compact: CompactLimits {
            threshold: 0.85,
            min_tokens,
            keep_tokens: 20_000,
            enabled: true,
            compactor_available: true,
        },
    }
}
fn inference(stop: Stop, calls: &[(&str, &str)], tokens: u64) -> Inference {
    let mut events = vec![StreamEvent::Usage(usage(tokens))];
    for (call, tool) in calls {
        events.push(StreamEvent::ToolCall {
            call: CallId::new(*call),
            name: (*tool).into(),
            args: RawJson::parse("{}").unwrap(),
        });
    }
    events.push(StreamEvent::Stop(stop));
    Inference { events }
}
fn stream_result(session: &mut Session, turn: TurnId, response: Inference) -> Vec<Effect> {
    send(
        session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(response),
            partial: None,
        },
    )
    .unwrap()
}
fn only_tool_result(records: &[Record]) -> &Entry {
    records
        .iter()
        .find_map(|record| match record {
            Record::ToolResult(entry) => Some(entry),
            _ => None,
        })
        .unwrap()
}
fn append_emitted(out: &[Effect], records: &mut Vec<Record>) {
    for effect in out {
        if let Effect::Emit(emit) = effect {
            records.extend(emit.records.iter().cloned());
        }
    }
}
fn same_replayed_state(live: &Session, records: &[Record]) {
    let replayed = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    assert_eq!(replayed.phase, live.phase);
    assert_eq!(replayed.tree, live.tree);
    assert_eq!(replayed.settings, live.settings);
    assert_eq!(replayed.allow_always, live.allow_always);
    assert_eq!(replayed.promoted, live.promoted);
    assert_eq!(replayed.next_turn, live.next_turn);
    assert_eq!(replayed.next_entry, live.next_entry);
    assert_eq!(replayed.last_turn, live.last_turn);
    assert_eq!(replayed.wake_run, live.wake_run);
    assert_eq!(replayed.projected_bytes, live.projected_bytes);
    assert_eq!(replayed.turn_totals, live.turn_totals);
}
fn one_read_round(session: &mut Session, turn: TurnId, call: &str, tokens: u64) -> Vec<Effect> {
    stream_result(
        session,
        turn,
        inference(Stop::EndTurn, &[(call, "read_file")], tokens),
    );
    let result = ResolvedCall {
        call: CallId::new(call),
        name: name("read_file"),
        promoted: false,
        result: Ok(ToolClass::Read),
    };
    send(
        session,
        Event::Resolved {
            turn,
            calls: vec![result],
            answerer_attached: false,
        },
    )
    .unwrap();
    send(
        session,
        Event::CallStarted {
            turn,
            call: CallId::new(call),
        },
    )
    .unwrap();
    send(
        session,
        Event::Settled {
            turn,
            call: CallId::new(call),
            outcome: SettledOutcome::Ok {
                text: "read".into(),
                data: None,
            },
        },
    )
    .unwrap()
}
fn compact_summary(before: u64, after: u64) -> CompactionSummary {
    CompactionSummary {
        compactor: name("summary"),
        tokens_before: before,
        tokens_after: after,
        summary: Some("kept context".into()),
        first_kept: Some(entry(1)),
        replay: None,
        usage: None,
        parts: Vec::new(),
        parts_tokens: 0,
        letters: Vec::new(),
    }
}

fn settle_manual_compaction(session: &mut Session, before: u64, after: u64) -> Vec<Effect> {
    let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
    send(
        session,
        Event::JobStarted {
            job,
            kind: JobKind::Compaction,
        },
    )
    .unwrap();
    let out = send(
        session,
        Event::CompactionSettled {
            turn: None,
            outcome: Ok(compact_summary(before, after)),
        },
    )
    .unwrap();
    send(
        session,
        Event::JobSettled {
            job,
            outcome: JobOutcome::Exited { code: 0 },
        },
    )
    .unwrap();
    out
}

#[test]
fn session_grant_accepts_a_mapped_tool_name() {
    let tool = "deploy.web-x.list";
    let request = RequestId::new_v7();
    let mut session = session();
    session.open_questions.push((
        request,
        super::types::QuestionRef {
            tool: Some(tool.into()),
        },
    ));
    session.preflight_session_grant(request).unwrap();
    session
        .grant_resolved(request, &Answer::ApproveForSession, true, false)
        .unwrap();
    assert!(
        session
            .allow_always()
            .contains(&Name::parse_mapped_tool(tool).unwrap())
    );
}

#[test]
fn replay_restores_mapped_tool_allowance() {
    let tool = "deploy.web-x.list";
    let replayed = Session::replay(
        [Record::AllowAlways {
            at: stamp(),
            tool: tool.into(),
            by: client(),
        }],
        stamp(),
    )
    .unwrap()
    .0;
    assert!(
        replayed
            .allow_always()
            .contains(&Name::parse_mapped_tool(tool).unwrap())
    );
}

#[test]
fn wrong_turn_changes_nothing() {
    let mut session = session();
    let before = session.clone();
    let mut out = vec![Effect::Reply(Ok(Reply::Done(Output::Nothing)))];
    let old_out = out.clone();
    let result = session.step(
        Event::Command {
            cmd: Command::Steer {
                turn: id(9),
                content: vec![Part::Text {
                    text: "late".into(),
                }],
            },
            by: client(),
        },
        stamp(),
        &mut out,
    );
    assert!(matches!(result, Err(Rejection::WrongTurn { .. })));
    assert_eq!(session, before);
    assert_eq!(out, old_out);
}

#[test]
fn steer_cell_holds_16() {
    let mut session = session();
    let turn = begin(&mut session);
    for index in 0..MAX_STEERS {
        send(
            &mut session,
            Event::Steer {
                turn,
                text: index.to_string().into(),
            },
        )
        .unwrap();
    }
    assert_eq!(
        send(
            &mut session,
            Event::Steer {
                turn,
                text: "overflow".into()
            }
        ),
        Err(Rejection::SteerFull)
    );
    assert_eq!(session.steers_queued(), 16);
}

#[test]
fn steer_at_final_response_extends_turn() {
    let mut session = session();
    let turn = begin(&mut session);
    stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
    let queued = send(
        &mut session,
        Event::Command {
            cmd: Command::Steer {
                turn,
                content: vec![Part::Text {
                    text: "continue".into(),
                }],
            },
            by: client(),
        },
    )
    .unwrap();
    assert!(matches!(
        queued.as_slice(),
        [Effect::Reply(Ok(Reply::Queued))]
    ));
    let boundary = send(&mut session, Event::Boundary { turn }).unwrap();
    assert!(
        boundary
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
    assert!(
        !boundary
            .iter()
            .any(|effect| matches!(effect, Effect::Stop { .. }))
    );
}

#[test]
fn cancel_discards_queued_input() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::Command {
            cmd: Command::FollowUp {
                turn,
                content: vec![Part::Text {
                    text: "first".into(),
                }],
            },
            by: client(),
        },
    )
    .unwrap();
    send(
        &mut session,
        Event::Steer {
            turn,
            text: "second".into(),
        },
    )
    .unwrap();
    send(
        &mut session,
        Event::Command {
            cmd: Command::FollowUp {
                turn,
                content: vec![Part::Text {
                    text: "third".into(),
                }],
            },
            by: client(),
        },
    )
    .unwrap();
    let out = send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: None,
        },
    )
    .unwrap();
    let emit = out
        .iter()
        .find_map(|effect| {
            if let Effect::Emit(emit) = effect {
                Some(emit)
            } else {
                None
            }
        })
        .unwrap();
    assert!(emit.updates.iter().any(|update| matches!(
        update,
        UpdateKind::TurnEnded {
            stop: Stop::Cancelled,
            ..
        }
    )));
    assert!(emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "discarded" && notice.text.as_ref() == "first\nsecond\nthird")));
    assert!(matches!(session.phase(), Phase::Idle));
}

#[test]
fn length_stop_fails_calls() {
    let mut session = session();
    let turn = begin(&mut session);
    let out = stream_result(
        &mut session,
        turn,
        inference(Stop::Length, &[("call", "edit_file")], 20),
    );
    let records = out
        .iter()
        .find_map(|effect| {
            if let Effect::Emit(emit) = effect {
                Some(&emit.records)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(
        only_tool_result(records).kind,
        EntryKind::ToolResult { error: true, .. }
    ));
    assert!(
        matches!(&only_tool_result(records).kind, EntryKind::ToolResult { parts, .. } if matches!(parts.first(), Some(JournalPart::Text { text }) if text.as_ref() == truncated_args("edit_file").as_ref()))
    );
    assert!(
        !out.iter()
            .any(|effect| matches!(effect, Effect::Dispatch { .. } | Effect::Stop { .. }))
    );
    assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    let mut no_calls = self::session();
    let no_call_turn = begin(&mut no_calls);
    let out = stream_result(
        &mut no_calls,
        no_call_turn,
        inference(Stop::Length, &[], 10),
    );
    assert!(out.iter().any(|effect| matches!(
        effect,
        Effect::Stop {
            stop: Stop::Length,
            ..
        }
    )));
}

#[test]
fn overflow_compacts_once_then_fails() {
    let mut session = session();
    send(&mut session, limits(0)).unwrap();
    let turn = begin(&mut session);
    let first = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Err(InferFailure::Overflow {
                code: "context".into(),
                message: "too large".into(),
            }),
            partial: None,
        },
    )
    .unwrap();
    assert!(first.iter().any(
        |effect| matches!(effect, Effect::Compact { turn: Some(active), .. } if *active == turn)
    ));
    let compacted = send(
        &mut session,
        Event::CompactionSettled {
            turn: Some(turn),
            outcome: Ok(compact_summary(100, 50)),
        },
    )
    .unwrap();
    assert_eq!(session.compactions(), 1);
    assert!(session.cache_key().ends_with(":1"));
    assert!(
        compacted
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
    let failed = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Err(InferFailure::Overflow {
                code: "context".into(),
                message: "still too large".into(),
            }),
            partial: None,
        },
    )
    .unwrap();
    assert!(failed.iter().any(|effect| matches!(
        effect,
        Effect::Stop {
            stop: Stop::Failed,
            ..
        }
    )));
    let emit = failed
        .iter()
        .find_map(|effect| {
            if let Effect::Emit(emit) = effect {
                Some(emit)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(
        emit.records.last(),
        Some(Record::TurnEnd { stop: TurnEndStop::Failed { message }, .. })
            if message.contains("Context overflow recovery failed")
    ));
}

#[test]
fn threshold_compaction_updates_cache_key() {
    let mut session = session();
    send(&mut session, limits(1)).unwrap();
    let turn = begin(&mut session);
    let out = one_read_round(&mut session, turn, "threshold", 85_100);
    assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_started" && notice.text.contains("85 percent of the window"))))));
    assert!(out.iter().any(
        |effect| matches!(effect, Effect::Compact { turn: Some(active), .. } if *active == turn)
    ));
    let out = send(
        &mut session,
        Event::CompactionSettled {
            turn: Some(turn),
            outcome: Ok(compact_summary(85_100, 20_000)),
        },
    )
    .unwrap();
    assert_eq!(session.compactions(), 1);
    assert!(session.cache_key().ends_with(":1"));
    assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
}

#[test]
fn missing_compactor_notifies_once_and_keeps_inference_running() {
    let mut session = session();
    send(
        &mut session,
        Event::Limits {
            window: 100_000,
            max_steps: 0,
            compact: CompactLimits {
                threshold: 0.85,
                min_tokens: 1,
                keep_tokens: 20_000,
                enabled: true,
                compactor_available: false,
            },
        },
    )
    .unwrap();
    let turn = begin(&mut session);
    let first = one_read_round(&mut session, turn, "first", 90_000);
    assert!(
        !first
            .iter()
            .any(|effect| matches!(effect, Effect::Compact { .. }))
    );
    assert!(
        first
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
    assert_eq!(first.iter().filter(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_none")))).count(), 1);

    let second = one_read_round(&mut session, turn, "second", 90_000);
    assert!(
        !second
            .iter()
            .any(|effect| matches!(effect, Effect::Compact { .. }))
    );
    assert!(
        second
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
    assert!(!second.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_none")))));
}

#[test]
fn breaker_after_three_automatic_failures() {
    let mut session = session();
    send(&mut session, limits(1)).unwrap();
    let turn = begin(&mut session);
    for index in 0..3 {
        let compact = one_read_round(&mut session, turn, &format!("call{index}"), 90_000);
        assert!(
            compact
                .iter()
                .any(|effect| matches!(effect, Effect::Compact { .. }))
        );
        let out = send(
            &mut session,
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Err("compactor unavailable".into()),
            },
        )
        .unwrap();
        if index == 2 {
            assert!(!session.auto_compaction_on());
            assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_off" && notice.text.contains("3 failed attempts"))))));
        }
    }
    send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: None,
        },
    )
    .unwrap();
    send(
        &mut session,
        Event::Command {
            cmd: Command::Compact { focus: None },
            by: client(),
        },
    )
    .unwrap();
    assert!(matches!(session.phase(), Phase::Compacting { .. }));
    let out = settle_manual_compaction(&mut session, 90_000, 20_000);
    assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_ended")))));
    assert!(session.auto_compaction_on());
}

#[test]
fn wake_limit_21st_refused() {
    let mut session = session();
    for _ in 0..20 {
        let mut out = Vec::new();
        session
            .step(
                Event::Wake {
                    text: "wake".into(),
                    sources: Box::new([]),
                    jobs: Box::new([]),
                },
                stamp(),
                &mut out,
            )
            .unwrap();
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("wake not opening: {phase:?}"),
        };
        session.step(guard(turn), stamp(), &mut out).unwrap();
        session
            .step(
                Event::Cancel {
                    scope: CancelScope::Turn(turn),
                    partial: None,
                },
                stamp(),
                &mut out,
            )
            .unwrap();
    }
    let mut out = vec![Effect::Reply(Ok(Reply::Done(Output::Nothing)))];
    let before = out.clone();
    assert!(matches!(
        session.step(
            Event::Wake {
                text: "refused".into(),
                sources: Box::new([]),
                jobs: Box::new([])
            },
            stamp(),
            &mut out
        ),
        Err(Rejection::Denied {
            reason: crate::approval::DenyReason::WakeLimit
        })
    ));
    assert_eq!(out, before);
    send(&mut session, prompt("user")).unwrap();
    assert_eq!(session.wake_run(), 0);
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        _ => panic!("user prompt not opening"),
    };
    send(&mut session, guard(turn)).unwrap();
    send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: None,
        },
    )
    .unwrap();
    assert!(
        send(
            &mut session,
            Event::Wake {
                text: "again".into(),
                sources: Box::new([]),
                jobs: Box::new([])
            }
        )
        .is_ok()
    );
}

#[test]
fn manual_compact_rejections() {
    let mut session = session();
    send(&mut session, limits(100)).unwrap();
    let turn = begin(&mut session);
    stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
    send(&mut session, Event::Boundary { turn }).unwrap();
    let out = send(
        &mut session,
        Event::Command {
            cmd: Command::Compact { focus: None },
            by: client(),
        },
    )
    .unwrap();
    assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compact.nothing")))));

    let mut session = self::session();
    send(&mut session, limits(1)).unwrap();
    let turn = begin(&mut session);
    stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
    send(&mut session, Event::Boundary { turn }).unwrap();
    send(
        &mut session,
        Event::Command {
            cmd: Command::Compact {
                focus: Some("auth flow".into()),
            },
            by: client(),
        },
    )
    .unwrap();
    assert!(matches!(
        send(&mut session, prompt("blocked")),
        Err(Rejection::Compacting)
    ));
    let compacted = settle_manual_compaction(&mut session, 10, 5);
    assert!(compacted.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_ended")))));
    let out = send(
        &mut session,
        Event::Command {
            cmd: Command::Compact { focus: None },
            by: client(),
        },
    )
    .unwrap();
    assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compact.already")))));
}

#[test]
fn stream_interrupt_replays_at_most_three() {
    let mut session = session();
    let turn = begin(&mut session);
    for index in 0..3 {
        let out = send(
            &mut session,
            Event::StreamVerdict {
                turn,
                verdict: StreamVerdict::Interrupt {
                    rule: format!("rule{index}").into(),
                    inject: format!("reminder{index}").into(),
                },
            },
        )
        .unwrap();
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
        assert!(out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.records.iter().any(|record| matches!(record, Record::RuleFired { .. })))));
    }
    let fourth = send(
        &mut session,
        Event::StreamVerdict {
            turn,
            verdict: StreamVerdict::Interrupt {
                rule: "last".into(),
                inject: "reminder".into(),
            },
        },
    )
    .unwrap();
    assert!(
        !fourth
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
    assert!(fourth.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "rule.suppressed")))));
}

#[test]
fn stream_reminder_persists_without_interrupt_update() {
    let mut session = session();
    let turn = begin(&mut session);
    let effects = send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "retry-control".into(),
            text: "<system-reminder>Respect the gate rule.</system-reminder>".into(),
        },
    )
    .unwrap();
    let emit = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Emit(emit) => Some(emit),
            _ => None,
        })
        .unwrap();
    assert!(emit.records.iter().any(|record| matches!(
        record,
        Record::Reminder(entry)
            if matches!(
                &entry.kind,
                EntryKind::Reminder { source, text }
                    if source.as_ref() == "rule:retry-control"
                        && text.contains("<system-reminder")
            )
    )));
    assert!(
        emit.updates
            .iter()
            .any(|update| matches!(update, UpdateKind::Tree(_)))
    );
    assert!(
        !emit
            .updates
            .iter()
            .any(|update| matches!(update, UpdateKind::RuleFired { .. }))
    );
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
}

#[test]
fn replay_repairs_aborted_turn() {
    let session_id = crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap();
    let call = CallId::new("call");
    let header = crate::journal::Header {
        id: session_id,
        at: stamp(),
        workspace: crate::workspace::Workspace::new(PathBuf::from("/")).unwrap(),
        product: crate::journal::Product::Dal,
        from: None,
    };
    let assistant = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test".into(),
            content: vec![Block::ToolCall {
                id: call.clone(),
                name: "read_file".into(),
                input: RawJson::parse("{}").unwrap(),
            }],
            usage: usage(1),
            stop: AssistantStop::ToolUse,
        },
    };
    let records = [
        Record::Session(header),
        Record::TurnStart {
            at: stamp(),
            turn: id(3),
        },
        Record::Assistant(assistant),
        Record::ToolStart {
            at: stamp(),
            turn: id(3),
            call,
        },
    ];
    let (session, effects) = Session::replay(records, stamp()).unwrap();
    assert!(matches!(session.phase(), Phase::Idle));
    assert_eq!(session.next_turn, Some(id(4)));
    let Effect::Emit(emit) = &effects[0] else {
        panic!("replay must append repair records");
    };
    assert!(matches!(
        emit.records.as_slice(),
        [
            Record::ToolResult(_),
            Record::TurnEnd {
                stop: TurnEndStop::Aborted,
                ..
            },
            Record::Boot { .. },
        ]
    ));
}

const LOST: &str = "Tool call was not completed: dalgon stopped before it finished.";

fn open_turn(session: &mut Session, journal: &mut Vec<Record>) -> TurnId {
    append_emitted(&send(session, prompt("question")).unwrap(), journal);
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    append_emitted(&send(session, guard(turn)).unwrap(), journal);
    turn
}
fn resolved_call(
    call: &str,
    tool: &str,
    promoted: bool,
    result: Result<ToolClass, ResolveError>,
) -> ResolvedCall {
    ResolvedCall {
        call: CallId::new(call),
        name: name(tool),
        promoted,
        result,
    }
}
fn results_of(records: &[Record], call: &str) -> Vec<(bool, Vec<JournalPart>)> {
    let call = CallId::new(call);
    records
        .iter()
        .filter_map(|record| match record {
            Record::ToolResult(entry) => match &entry.kind {
                EntryKind::ToolResult {
                    call: result,
                    error,
                    parts,
                    ..
                } if *result == call => Some((*error, parts.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
fn text_part(text: &str) -> Vec<JournalPart> {
    vec![JournalPart::Text { text: text.into() }]
}
fn assistant_record(entry_id: u64, calls: &[&str]) -> Record {
    Record::Assistant(Entry {
        id: entry(entry_id),
        parent: None,
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test".into(),
            content: calls
                .iter()
                .map(|call| Block::ToolCall {
                    id: CallId::new(*call),
                    name: "read_file".into(),
                    input: RawJson::parse("{}").unwrap(),
                })
                .collect(),
            usage: usage(1),
            stop: AssistantStop::ToolUse,
        },
    })
}
fn result_record(entry_id: u64, call: &str) -> Record {
    Record::ToolResult(Entry {
        id: entry(entry_id),
        parent: None,
        at: stamp(),
        kind: EntryKind::ToolResult {
            call: CallId::new(call),
            name: "read_file".into(),
            error: false,
            parts: text_part("ok"),
            changes: Vec::new(),
        },
    })
}
fn start_record(turn: u64, call: &str) -> Record {
    Record::ToolStart {
        at: stamp(),
        turn: id(turn),
        call: CallId::new(call),
    }
}
fn turn_start_record(turn: u64) -> Record {
    Record::TurnStart {
        at: stamp(),
        turn: id(turn),
    }
}
fn turn_end_record(turn: u64) -> Record {
    Record::TurnEnd {
        at: stamp(),
        turn: id(turn),
        stop: TurnEndStop::Done,
        usage: None,
        changes: Vec::new(),
    }
}
fn contradicts(records: Vec<Record>) -> bool {
    matches!(
        Session::replay(records, stamp()),
        Err(ReplayError::Contradiction { .. })
    )
}

#[test]
fn promoted_call_runs_and_promotes_only_after_success() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let promotions = |journal: &[Record]| {
        journal
            .iter()
            .filter(|record| matches!(record, Record::ToolPromoted { .. }))
            .count()
    };
    for (round, call) in ["first", "again"].into_iter().enumerate() {
        append_emitted(
            &stream_result(
                &mut session,
                turn,
                inference(Stop::EndTurn, &[(call, "web_search")], 10),
            ),
            &mut journal,
        );
        let resolved = send(
            &mut session,
            Event::Resolved {
                turn,
                calls: vec![resolved_call(call, "web_search", true, Ok(ToolClass::Read))],
                answerer_attached: false,
            },
        )
        .unwrap();
        append_emitted(&resolved, &mut journal);
        assert!(resolved.iter().any(|effect| matches!(effect, Effect::Dispatch { units, .. }
            if matches!(units.as_slice(), [Unit::Reads { calls }] if *calls == [CallId::new(call)]))));
        assert_eq!(promotions(&journal), round);
        assert_eq!(session.promoted().contains(&name("web_search")), round > 0);
        assert!(results_of(&journal, call).is_empty());

        append_emitted(
            &send(
                &mut session,
                Event::CallStarted {
                    turn,
                    call: CallId::new(call),
                },
            )
            .unwrap(),
            &mut journal,
        );
        let settled = send(
            &mut session,
            Event::Settled {
                turn,
                call: CallId::new(call),
                outcome: SettledOutcome::Ok {
                    text: "found".into(),
                    data: None,
                },
            },
        )
        .unwrap();
        append_emitted(&settled, &mut journal);
        assert!(
            settled
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert_eq!(
            results_of(&journal, call),
            vec![(false, text_part("found"))]
        );
        assert_eq!(promotions(&journal), 1);
        assert!(session.promoted().contains(&name("web_search")));
    }
    let result_at = journal
        .iter()
        .position(|record| matches!(record, Record::ToolResult(_)))
        .unwrap();
    let promoted_at = journal
        .iter()
        .position(|record| {
            matches!(record, Record::ToolPromoted { tool, turn: Some(owner), .. }
        if tool.as_ref() == "web_search" && *owner == turn)
        })
        .unwrap();
    assert!(result_at < promoted_at);
    append_emitted(
        &stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10)),
        &mut journal,
    );
    append_emitted(
        &send(&mut session, Event::Boundary { turn }).unwrap(),
        &mut journal,
    );
    assert!(matches!(session.phase(), Phase::Idle));
    same_replayed_state(&session, &journal);
    let (replayed, _) = Session::replay(journal, stamp()).unwrap();
    assert!(replayed.promoted().contains(&name("web_search")));
}

#[test]
fn unsuccessful_promoting_calls_do_not_promote() {
    let outcomes = [
        SettledOutcome::Err {
            text: "failed".into(),
        },
        SettledOutcome::Interrupted,
        SettledOutcome::Detached {
            job: JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap(),
        },
    ];
    for outcome in outcomes {
        let mut session = session();
        let mut journal = Vec::new();
        let turn = open_turn(&mut session, &mut journal);
        append_emitted(
            &stream_result(
                &mut session,
                turn,
                inference(Stop::EndTurn, &[("call", "web_search")], 10),
            ),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::Resolved {
                    turn,
                    calls: vec![resolved_call(
                        "call",
                        "web_search",
                        true,
                        Ok(ToolClass::Read),
                    )],
                    answerer_attached: false,
                },
            )
            .unwrap(),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::CallStarted {
                    turn,
                    call: CallId::new("call"),
                },
            )
            .unwrap(),
            &mut journal,
        );
        append_emitted(
            &send(
                &mut session,
                Event::Settled {
                    turn,
                    call: CallId::new("call"),
                    outcome,
                },
            )
            .unwrap(),
            &mut journal,
        );
        assert_eq!(results_of(&journal, "call").len(), 1);
        assert!(
            !journal
                .iter()
                .any(|record| matches!(record, Record::ToolPromoted { .. }))
        );
        assert!(session.promoted().is_empty());
    }
}

#[test]
fn every_resolved_call_gets_exactly_one_result() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let provider_calls = [
        ("unknown", "nope"),
        ("eval", "cell_only"),
        ("bad", "read_file"),
        ("dup", "read_file"),
        ("promote", "web_search"),
        ("plain", "read_file"),
    ];
    append_emitted(
        &stream_result(
            &mut session,
            turn,
            inference(Stop::EndTurn, &provider_calls, 10),
        ),
        &mut journal,
    );
    let resolved = send(
        &mut session,
        Event::Resolved {
            turn,
            calls: vec![
                resolved_call("unknown", "nope", false, Err(ResolveError::Unknown)),
                resolved_call("eval", "cell_only", true, Err(ResolveError::EvalOnly)),
                resolved_call(
                    "bad",
                    "read_file",
                    false,
                    Err(ResolveError::InvalidArgs("missing path".into())),
                ),
                resolved_call("dup", "read_file", false, Ok(ToolClass::Read)),
                resolved_call("dup", "read_file", false, Ok(ToolClass::Read)),
                resolved_call("promote", "web_search", true, Ok(ToolClass::Read)),
                resolved_call("plain", "read_file", false, Ok(ToolClass::Read)),
            ],
            answerer_attached: false,
        },
    )
    .unwrap();
    append_emitted(&resolved, &mut journal);
    assert!(resolved.iter().any(|effect| matches!(effect, Effect::Dispatch { units, .. }
        if matches!(units.as_slice(), [Unit::Reads { calls }] if *calls == [CallId::new("promote"), CallId::new("plain")]))));
    assert!(
        !journal
            .iter()
            .any(|record| matches!(record, Record::ToolPromoted { .. }))
    );
    for call in ["promote", "plain"] {
        let started = Event::CallStarted {
            turn,
            call: CallId::new(call),
        };
        append_emitted(&send(&mut session, started).unwrap(), &mut journal);
        let settled = Event::Settled {
            turn,
            call: CallId::new(call),
            outcome: SettledOutcome::Ok {
                text: call.into(),
                data: None,
            },
        };
        append_emitted(&send(&mut session, settled).unwrap(), &mut journal);
    }
    append_emitted(
        &stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10)),
        &mut journal,
    );
    append_emitted(
        &send(&mut session, Event::Boundary { turn }).unwrap(),
        &mut journal,
    );
    assert_eq!(journal.iter().filter(|record| matches!(record, Record::ToolPromoted { tool, .. } if tool.as_ref() == "web_search")).count(), 1);
    assert_eq!(
        session.promoted().iter().collect::<Vec<_>>(),
        vec![&name("web_search")]
    );
    assert!(matches!(session.phase(), Phase::Idle));

    let expected = [
        ("unknown", true, "unknown tool: nope"),
        ("eval", true, "cell_only is callable only from eval cells"),
        ("bad", true, "invalid arguments for read_file: missing path"),
        (
            "dup",
            true,
            "invalid arguments for read_file: duplicate call id dup",
        ),
        ("promote", false, "promote"),
        ("plain", false, "plain"),
    ];
    for (call, error, text) in expected {
        assert_eq!(
            results_of(&journal, call),
            vec![(error, text_part(text))],
            "{call}"
        );
    }
    same_replayed_state(&session, &journal);
}

#[test]
fn replay_rejects_unpaired_tool_calls_and_results() {
    // A second result for one call.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        start_record(1, "a"),
        result_record(2, "a"),
        result_record(3, "a"),
        turn_end_record(1),
    ]));
    // A repeated result after its turn ended.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        result_record(2, "a"),
        turn_end_record(1),
        result_record(3, "a"),
    ]));
    // A result that answers no assistant call.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        result_record(2, "b"),
        result_record(3, "a"),
        turn_end_record(1),
    ]));
    // A started call whose turn completes without its result.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        start_record(1, "a"),
        turn_end_record(1)
    ]));
    // A never-started call whose turn completes without its result.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a", "b"]),
        result_record(2, "a"),
        turn_end_record(1),
    ]));
    // A start for a call no response made, and a start repeated for one call.
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        start_record(1, "b")
    ]));
    assert!(contradicts(vec![
        turn_start_record(1),
        assistant_record(1, &["a"]),
        start_record(1, "a"),
        start_record(1, "a")
    ]));

    // Resolution-error results have no start, and one response may repeat an id.
    let (session, effects) = Session::replay(
        vec![
            turn_start_record(1),
            assistant_record(1, &["a", "a", "b"]),
            result_record(2, "a"),
            start_record(1, "b"),
            result_record(3, "b"),
            turn_end_record(1),
        ],
        stamp(),
    )
    .unwrap();
    assert!(matches!(session.phase(), Phase::Idle));
    let [Effect::Emit(emit)] = effects.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert!(matches!(emit.records.as_slice(), [Record::Boot { .. }]));
}

#[test]
fn replay_repairs_every_open_call_once() {
    let mut records = vec![
        turn_start_record(3),
        assistant_record(1, &["ran", "failed", "queued"]),
        start_record(3, "ran"),
        result_record(2, "failed"),
    ];
    let (session, effects) = Session::replay(records.clone(), stamp()).unwrap();
    assert!(matches!(session.phase(), Phase::Idle));
    assert_eq!(session.next_turn, Some(id(4)));
    let [Effect::Emit(repair)] = effects.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert!(matches!(
        repair.records.as_slice(),
        [
            Record::ToolResult(_),
            Record::ToolResult(_),
            Record::TurnEnd {
                stop: TurnEndStop::Aborted,
                ..
            },
            Record::Boot { .. },
        ]
    ));
    assert_eq!(
        results_of(&repair.records, "ran"),
        vec![(true, text_part(LOST))]
    );
    assert_eq!(
        results_of(&repair.records, "queued"),
        vec![(true, text_part(LOST))]
    );
    assert!(results_of(&repair.records, "failed").is_empty());
    assert!(matches!(&repair.records[0], Record::ToolResult(entry)
        if matches!(&entry.kind, EntryKind::ToolResult { call, .. } if *call == CallId::new("ran"))));

    records.extend(repair.records.iter().cloned());
    let (reopened, again) = Session::replay(records, stamp()).unwrap();
    assert!(matches!(reopened.phase(), Phase::Idle));
    let [Effect::Emit(second)] = again.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert!(matches!(second.records.as_slice(), [Record::Boot { .. }]));
}

#[test]
fn promoted_call_lost_in_crash_is_repaired_once() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    append_emitted(
        &stream_result(
            &mut session,
            turn,
            inference(Stop::EndTurn, &[("call", "web_search")], 10),
        ),
        &mut journal,
    );
    append_emitted(
        &send(
            &mut session,
            Event::Resolved {
                turn,
                calls: vec![resolved_call(
                    "call",
                    "web_search",
                    true,
                    Ok(ToolClass::Read),
                )],
                answerer_attached: false,
            },
        )
        .unwrap(),
        &mut journal,
    );
    append_emitted(
        &send(
            &mut session,
            Event::CallStarted {
                turn,
                call: CallId::new("call"),
            },
        )
        .unwrap(),
        &mut journal,
    );

    let (replayed, effects) = Session::replay(journal.clone(), stamp()).unwrap();
    assert!(replayed.promoted().is_empty());
    assert!(
        !journal
            .iter()
            .any(|record| matches!(record, Record::ToolPromoted { .. }))
    );
    let [Effect::Emit(repair)] = effects.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert_eq!(
        results_of(&repair.records, "call"),
        vec![(true, text_part(LOST))]
    );
    assert!(matches!(
        repair.records.as_slice(),
        [
            Record::ToolResult(_),
            Record::TurnEnd {
                stop: TurnEndStop::Aborted,
                ..
            },
            Record::Boot { .. },
        ]
    ));

    journal.extend(repair.records.iter().cloned());
    let (_, again) = Session::replay(journal.clone(), stamp()).unwrap();
    let [Effect::Emit(second)] = again.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert!(results_of(&second.records, "call").is_empty());
    assert_eq!(results_of(&journal, "call").len(), 1);
}

#[test]
fn cloned_entries_without_turn_records_replay() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    append_emitted(
        &stream_result(
            &mut session,
            turn,
            inference(Stop::EndTurn, &[("call", "read_file")], 10),
        ),
        &mut journal,
    );
    let header = crate::journal::Header {
        id: crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap(),
        at: stamp(),
        workspace: crate::workspace::Workspace::new(PathBuf::from("/")).unwrap(),
        product: crate::journal::Product::Dal,
        from: None,
    };
    let open = crate::journal::branch(
        &journal,
        session.tree.leaf,
        crate::journal::BranchMode::Clone,
        &header,
    )
    .unwrap();
    assert!(
        !open
            .records
            .iter()
            .any(|record| matches!(record, Record::TurnStart { .. }))
    );
    let (_, effects) = Session::replay(open.records, stamp()).unwrap();
    let [Effect::Emit(repair)] = effects.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert!(matches!(repair.records.as_slice(), [Record::Boot { .. }]));

    append_emitted(
        &send(
            &mut session,
            Event::Resolved {
                turn,
                calls: vec![resolved_call(
                    "call",
                    "read_file",
                    false,
                    Ok(ToolClass::Read),
                )],
                answerer_attached: false,
            },
        )
        .unwrap(),
        &mut journal,
    );
    append_emitted(
        &send(
            &mut session,
            Event::CallStarted {
                turn,
                call: CallId::new("call"),
            },
        )
        .unwrap(),
        &mut journal,
    );
    append_emitted(
        &send(
            &mut session,
            Event::Settled {
                turn,
                call: CallId::new("call"),
                outcome: SettledOutcome::Ok {
                    text: "read".into(),
                    data: None,
                },
            },
        )
        .unwrap(),
        &mut journal,
    );
    let settled = crate::journal::branch(
        &journal,
        session.tree.leaf,
        crate::journal::BranchMode::Clone,
        &header,
    )
    .unwrap();
    assert_eq!(results_of(&settled.records, "call").len(), 1);
    Session::replay(settled.records, stamp()).unwrap();
}

#[test]
fn replay_restores_wake_count() {
    let (mut session, _) = Session::replay(
        [Record::WakeAttempt {
            at: stamp(),
            turn: id(1),
            count: 7,
            jobs: Vec::new(),
        }],
        stamp(),
    )
    .unwrap();
    assert_eq!(session.wake_run(), 7);
    for _ in 0..13 {
        let mut out = Vec::new();
        session
            .step(
                Event::Wake {
                    text: "wake".into(),
                    sources: Box::new([]),
                    jobs: Box::new([]),
                },
                stamp(),
                &mut out,
            )
            .unwrap();
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            _ => panic!("wake not opening"),
        };
        session.step(guard(turn), stamp(), &mut out).unwrap();
        session
            .step(
                Event::Cancel {
                    scope: CancelScope::Turn(turn),
                    partial: None,
                },
                stamp(),
                &mut out,
            )
            .unwrap();
    }
    assert_eq!(session.wake_run(), 20);
    assert!(matches!(
        send(
            &mut session,
            Event::Wake {
                text: "refused".into(),
                sources: Box::new([]),
                jobs: Box::new([])
            }
        ),
        Err(Rejection::Denied {
            reason: crate::approval::DenyReason::WakeLimit
        })
    ));
}

proptest::proptest! {
    #[test]
    fn settlement_invariants_hold_on_random_sequences(actions in proptest::collection::vec(0_u8..3, 0..40)) {
        let mut session = session();
        let mut journal = Vec::new();
        for (index, action) in actions.into_iter().enumerate() {
            if action == 0 {
                let before = session.clone();
                let mut out = vec![Effect::Reply(Ok(Reply::Done(Output::Nothing)))];
                let original = out.clone();
                let rejected = session.step(Event::Command {
                    cmd: Command::Steer { turn: id(99), content: Vec::new() },
                    by: client(),
                }, stamp(), &mut out);
                proptest::prop_assert!(rejected.is_err());
                proptest::prop_assert_eq!(&session, &before);
                proptest::prop_assert_eq!(&out, &original);
                same_replayed_state(&session, &journal);
                continue;
            }
            let prompt_out = send(&mut session, prompt(&format!("q{index}"))).unwrap();
            append_emitted(&prompt_out, &mut journal);
            let turn = match session.phase() { Phase::Opening { turn, .. } => *turn, phase => panic!("expected opening, got {phase:?}") };
            let guard_out = send(&mut session, guard(turn)).unwrap();
            append_emitted(&guard_out, &mut journal);
            let cancel_out = send(&mut session, Event::Cancel { scope: CancelScope::Turn(turn), partial: None }).unwrap();
            append_emitted(&cancel_out, &mut journal);
            proptest::prop_assert!(matches!(session.phase(), Phase::Idle));
            let starts = journal.iter().filter(|record| matches!(record, Record::TurnStart { .. })).count();
            let ends = journal.iter().filter(|record| matches!(record, Record::TurnEnd { .. })).count();
            proptest::prop_assert_eq!(starts, ends);
            same_replayed_state(&session, &journal);
        }
    }
}

#[test]
fn interrupted_stream_records_partial_assistant_and_call_result_once() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::RequestStarted {
            turn,
            model: route(),
            family: Family::Chat,
        },
    )
    .unwrap();
    let call = CallId::new("partial-call");
    let partial = PartialResponse {
        content: vec![
            Block::Text {
                text: "partial answer".into(),
            },
            Block::ToolCall {
                id: call.clone(),
                name: "read_file".into(),
                input: RawJson::parse("{}").unwrap(),
            },
        ],
        usage: usage(7),
    };
    let out = send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: Some(partial),
        },
    )
    .unwrap();
    let mut records = Vec::new();
    append_emitted(&out, &mut records);
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::Assistant(_)))
            .count(),
        1
    );
    assert_eq!(records.iter().filter(|record| matches!(record, Record::ToolResult(entry) if matches!(&entry.kind, EntryKind::ToolResult { call: result, .. } if result == &call))).count(), 1);
    assert!(matches!(
        records.last(),
        Some(Record::TurnEnd {
            stop: TurnEndStop::Cancelled,
            ..
        })
    ));

    let late = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Err(InferFailure::Cancelled),
            partial: None,
        },
    )
    .unwrap();
    assert!(late.is_empty());
}

#[test]
fn boundary_waits_for_resolution_before_resuming_model() {
    let mut session = session();
    let turn = begin(&mut session);
    stream_result(
        &mut session,
        turn,
        inference(Stop::EndTurn, &[("call", "read_file")], 10),
    );
    assert!(matches!(
        session.phase(),
        Phase::Running {
            stage: TurnStage::Resolving { .. },
            ..
        }
    ));

    let before = session.clone();
    let early = send(&mut session, Event::Boundary { turn }).unwrap();
    assert_eq!(session, before);
    assert!(
        !early
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_) | Effect::Dispatch { .. }))
    );

    let resolved = send(
        &mut session,
        Event::Resolved {
            turn,
            calls: vec![ResolvedCall {
                call: CallId::new("call"),
                name: name("read_file"),
                promoted: false,
                result: Ok(ToolClass::Read),
            }],
            answerer_attached: false,
        },
    )
    .unwrap();
    assert!(resolved.iter().any(
        |effect| matches!(effect, Effect::Dispatch { turn: dispatched, .. } if *dispatched == turn)
    ));
    assert!(
        !resolved
            .iter()
            .any(|effect| matches!(effect, Effect::Infer(_)))
    );
}

#[test]
fn queued_wakes_keep_unique_turns_and_replay_their_count() {
    let mut session = session();
    let mut journal = Vec::new();
    let prompt_out = send(&mut session, prompt("question")).unwrap();
    append_emitted(&prompt_out, &mut journal);
    let first_turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    let opened = send(&mut session, guard(first_turn)).unwrap();
    append_emitted(&opened, &mut journal);

    for text in ["first wake", "second wake"] {
        let out = send(
            &mut session,
            Event::Wake {
                text: text.into(),
                sources: Box::new([]),
                jobs: Box::new([]),
            },
        )
        .unwrap();
        append_emitted(&out, &mut journal);
    }
    let ended = stream_result(&mut session, first_turn, inference(Stop::EndTurn, &[], 10));
    append_emitted(&ended, &mut journal);
    let boundary = send(&mut session, Event::Boundary { turn: first_turn }).unwrap();
    append_emitted(&boundary, &mut journal);
    let wake_turn = match session.phase() {
        Phase::Settling {
            follow_up: Some((turn, TurnSource::Wake { .. })),
            ..
        } => *turn,
        phase => panic!("first queued wake did not settle next: {phase:?}"),
    };
    let opened = send(&mut session, guard(wake_turn)).unwrap();
    append_emitted(&opened, &mut journal);
    let cancelled = send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(wake_turn),
            partial: None,
        },
    )
    .unwrap();
    append_emitted(&cancelled, &mut journal);

    let attempts = journal
        .iter()
        .filter_map(|record| match record {
            Record::WakeAttempt { turn, count, .. } => Some((*turn, *count)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(attempts, vec![(id(2), 1), (id(3), 2)]);
    let starts = journal
        .iter()
        .filter_map(|record| match record {
            Record::TurnStart { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(starts, vec![id(1), id(2)]);
    assert_eq!(session.next_turn, Some(id(4)));
    assert_eq!(session.wake_run(), 2);
    same_replayed_state(&session, &journal);
}

#[test]
fn replay_accepts_legacy_job_start_without_kind() {
    let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
    let records = [Record::Job {
        at: stamp(),
        job,
        event: JobEvent::Started { kind: None },
    }];
    let (_, effects) = Session::replay(records, stamp()).unwrap();
    let [Effect::Emit(emit)] = effects.as_slice() else {
        panic!("replay must return its boot and orphan repair records");
    };
    assert!(matches!(emit.records.as_slice(), [Record::Job {
        job: orphaned,
        event: JobEvent::Orphaned,
        ..
    }, Record::Boot { .. }] if *orphaned == job));
}

#[test]
fn blob_journal_parts_preserve_stored_lengths() {
    let blob = crate::BlobId::from_bytes(b"stored payload");
    let text = part_to_journal(&Part::Blob {
        blob_id: blob,
        mime: "text/plain".into(),
        bytes: 13,
    });
    assert!(matches!(text, JournalPart::TextBlob { bytes: 13, .. }));
    let image = part_to_journal(&Part::Blob {
        blob_id: blob,
        mime: "image/png".into(),
        bytes: 29,
    });
    assert!(matches!(image, JournalPart::ImageBlob { bytes: 29, .. }));
    let other = part_to_journal(&Part::Blob {
        blob_id: blob,
        mime: "application/pdf".into(),
        bytes: 41,
    });
    assert!(
        matches!(other, JournalPart::Blob { bytes: 41, mime, .. } if mime.as_ref() == "application/pdf")
    );
}

proptest::proptest! {
    #[test]
    fn every_dispatched_call_settles_once_and_replays(count in 1_usize..8) {
        let mut session = session();
        let mut journal = Vec::new();
        let prompt_out = send(&mut session, prompt("question")).unwrap();
        append_emitted(&prompt_out, &mut journal);
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        let opened = send(&mut session, guard(turn)).unwrap();
        append_emitted(&opened, &mut journal);
        let names = (0..count).map(|index| format!("call{index}")).collect::<Vec<_>>();
        let call_defs = names.iter().map(|call| (call.as_str(), "read_file")).collect::<Vec<_>>();
        let response = stream_result(&mut session, turn, inference(Stop::EndTurn, &call_defs, 10));
        append_emitted(&response, &mut journal);
        let resolved = names.iter().map(|call| ResolvedCall {
            call: CallId::new(call.as_str()),
            name: name("read_file"),
            promoted: false,
            result: Ok(ToolClass::Read),
        }).collect::<Vec<_>>();
        let dispatched = send(&mut session, Event::Resolved {
            turn,
            calls: resolved,
            answerer_attached: false,
        }).unwrap();
        append_emitted(&dispatched, &mut journal);
        let dispatched_this_turn = dispatched.iter().any(|effect| matches!(effect, Effect::Dispatch { turn: dispatched_turn, .. } if *dispatched_turn == turn));
        proptest::prop_assert!(dispatched_this_turn);
        let units = dispatched.iter().find_map(|effect| match effect {
            Effect::Dispatch { units, .. } => Some(units.as_slice()),
            _ => None,
        }).unwrap();
        for call in &names {
            let started = send(&mut session, Event::CallStarted {
                turn,
                call: CallId::new(call.as_str()),
            }).unwrap();
            append_emitted(&started, &mut journal);
            let duplicate = send(&mut session, Event::CallStarted {
                turn,
                call: CallId::new(call.as_str()),
            }).unwrap();
            proptest::prop_assert!(duplicate.is_empty());
        }

        for call in &names {
            let settled = send(&mut session, Event::Settled {
                turn,
                call: CallId::new(call.as_str()),
                outcome: SettledOutcome::Ok { text: "result".into(), data: None },
            }).unwrap();
            append_emitted(&settled, &mut journal);
        }
        let final_response = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
        append_emitted(&final_response, &mut journal);
        let ended = send(&mut session, Event::Boundary { turn }).unwrap();
        append_emitted(&ended, &mut journal);
        proptest::prop_assert!(matches!(session.phase(), Phase::Idle));

        for call in &names {
            let id = CallId::new(call.as_str());
            let planned = units.iter().map(|unit| match unit {
                Unit::Reads { calls } => calls.iter().filter(|candidate| *candidate == &id).count(),
                Unit::Serial { call } => usize::from(call == &id),
            }).sum::<usize>();
            let starts = journal.iter().filter(|record| matches!(record, Record::ToolStart { call: started, .. } if started == &id)).count();
            let results = journal.iter().filter(|record| matches!(record, Record::ToolResult(entry) if matches!(&entry.kind, EntryKind::ToolResult { call: result, .. } if result == &id))).count();
            proptest::prop_assert_eq!(starts, 1);
            proptest::prop_assert_eq!(planned, 1);
            proptest::prop_assert_eq!(results, 1);
        }
        let turn_starts = journal.iter().filter(|record| matches!(record, Record::TurnStart { .. })).count();
        let turn_ends = journal.iter().filter(|record| matches!(record, Record::TurnEnd { .. })).count();
        proptest::prop_assert_eq!(turn_starts, turn_ends);
        same_replayed_state(&session, &journal);
    }
}

#[test]
fn receipt_effect_precedes_reply_and_publish() {
    let mut session = session();
    let out = send(
        &mut session,
        Event::Command {
            cmd: Command::SetThinking {
                level: ThinkingLevel::High,
                save: crate::command::Save::SessionOnly,
            },
            by: client(),
        },
    )
    .unwrap();
    assert!(
        matches!(out.first(), Some(Effect::Emit(emit)) if !emit.records.is_empty() && !emit.updates.is_empty())
    );
    assert!(matches!(
        out.last(),
        Some(Effect::Reply(Ok(Reply::Done(Output::Nothing))))
    ));
}
#[test]
fn set_mode_persists_and_replays() {
    let mut session = session();
    let out = send(
        &mut session,
        Event::Command {
            cmd: Command::SetMode {
                mode: Mode::EvalFirst,
                save: crate::command::Save::SessionOnly,
            },
            by: client(),
        },
    )
    .unwrap();
    assert!(matches!(
        out.last(),
        Some(Effect::Reply(Ok(Reply::Done(Output::Nothing))))
    ));
    let stored = out.iter().find_map(|effect| match effect {
        Effect::Emit(emit) => emit
            .records
            .iter()
            .find(|record| matches!(record, Record::Mode(_))),
        _ => None,
    });
    assert!(matches!(stored, Some(Record::Mode(_))));
    assert_eq!(session.settings_view().mode, Mode::EvalFirst);
}

#[test]
fn prompt_journals_nothing_until_before_turn_verdict() {
    let mut session = session();
    let out = send(&mut session, prompt("question")).unwrap();
    assert!(matches!(
        out.as_slice(),
        [Effect::Reply(Ok(Reply::Accepted { .. }))]
    ));
    assert!(matches!(session.phase(), Phase::Opening { .. }));
}

#[test]
fn wake_opens_with_wake_source() {
    let mut session = session();
    let out = send(
        &mut session,
        Event::Wake {
            text: "wake".into(),
            sources: Box::new([]),
            jobs: Box::new([]),
        },
    )
    .unwrap();
    assert!(!out.iter().any(|effect| matches!(effect, Effect::Reply(_))));
    assert!(matches!(
        session.phase(),
        Phase::Opening {
            source: TurnSource::Wake { .. },
            ..
        }
    ));
}

#[test]
fn before_turn_texts_join_into_user_entry() {
    let mut session = session();
    send(&mut session, prompt("question")).unwrap();
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    let verdict = send(
        &mut session,
        Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome: HookOutcome::new(
                HookEvent::BeforeTurn,
                HookVerdict::BeforeTurn(Some("first\nsecond".into())),
            )
            .unwrap(),
        },
    )
    .unwrap();
    let emit = verdict
        .iter()
        .find_map(|effect| match effect {
            Effect::Emit(emit) => Some(emit),
            _ => None,
        })
        .expect("before_turn verdict emits records");
    assert!(matches!(
        emit.records.as_slice(),
        [Record::TurnStart { turn: started, .. }, Record::User(_)]
        if *started == turn
    ));
    let entry = emit
        .records
        .iter()
        .find_map(|record| match record {
            Record::User(entry) => Some(entry),
            _ => None,
        })
        .expect("user entry journaled");
    let EntryKind::User { parts } = &entry.kind else {
        panic!("user record holds a user entry");
    };
    assert!(matches!(
        parts.as_slice(),
        [JournalPart::Text { text: first }, JournalPart::Text { text: second }]
        if first.as_ref() == "question" && second.as_ref() == "\n\nfirst\nsecond"
    ));
    assert!(emit.updates.iter().any(
        |update| matches!(update, UpdateKind::TurnStarted { turn: started, .. } if *started == turn)
    ));
    assert!(
        emit.updates
            .iter()
            .any(|update| matches!(update, UpdateKind::Tree(_)))
    );
    assert!(matches!(session.phase(), Phase::Running { .. }));
}

#[test]
fn turn_end_carries_accumulated_response_usage() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let streamed = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
    let ended = send(&mut session, Event::Boundary { turn }).unwrap();
    append_emitted(&streamed, &mut journal);
    append_emitted(&ended, &mut journal);
    let end = journal
        .iter()
        .find_map(|record| match record {
            Record::TurnEnd {
                turn: ended,
                usage,
                changes,
                ..
            } if *ended == turn => Some((usage, changes)),
            _ => None,
        })
        .expect("turn ends with a TurnEnd record");
    assert_eq!(end.0, &Some(usage(10)));
    assert!(end.1.is_empty());
    assert!(journal.iter().any(|record| matches!(
        record,
        Record::TurnEnd {
            stop: TurnEndStop::Done,
            ..
        }
    )));
    assert!(matches!(session.phase(), Phase::Idle));
}

#[test]
fn turn_end_sums_usage_across_tool_rounds() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let first = stream_result(
        &mut session,
        turn,
        inference(Stop::EndTurn, &[("call", "read_file")], 10),
    );
    append_emitted(&first, &mut journal);
    let resolved = send(
        &mut session,
        Event::Resolved {
            turn,
            calls: vec![resolved_call(
                "call",
                "read_file",
                false,
                Ok(ToolClass::Read),
            )],
            answerer_attached: false,
        },
    )
    .unwrap();
    append_emitted(&resolved, &mut journal);
    let started = send(
        &mut session,
        Event::CallStarted {
            turn,
            call: CallId::new("call"),
        },
    )
    .unwrap();
    append_emitted(&started, &mut journal);
    let settled = send(
        &mut session,
        Event::Settled {
            turn,
            call: CallId::new("call"),
            outcome: SettledOutcome::Ok {
                text: "result".into(),
                data: None,
            },
        },
    )
    .unwrap();
    append_emitted(&settled, &mut journal);
    let second = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 7));
    append_emitted(&second, &mut journal);
    let ended = send(&mut session, Event::Boundary { turn }).unwrap();
    append_emitted(&ended, &mut journal);
    let usage = journal
        .iter()
        .find_map(|record| match record {
            Record::TurnEnd {
                turn: ended, usage, ..
            } if *ended == turn => Some(*usage),
            _ => None,
        })
        .expect("turn ends with a TurnEnd record");
    assert_eq!(
        usage,
        Some(Usage {
            input_tokens: 17,
            cached_input_tokens: 0,
            output_tokens: 2,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        })
    );
    assert!(matches!(session.phase(), Phase::Idle));
    same_replayed_state(&session, &journal);
}

fn inferred_event(tokens: u64) -> Event {
    Event::Inferred {
        at: stamp(),
        who: crate::Owner::Core,
        purpose: crate::InferredPurpose::Synthetic {
            id: "test/model".into(),
        },
        usage: usage(tokens),
    }
}

#[test]
fn inferred_usage_counts_only_inside_the_running_turn_and_replays() {
    let mut session = session();
    let mut journal = Vec::new();
    let before = send(&mut session, inferred_event(100)).unwrap();
    append_emitted(&before, &mut journal);
    let turn = open_turn(&mut session, &mut journal);
    let inside = send(&mut session, inferred_event(11)).unwrap();
    append_emitted(&inside, &mut journal);
    let response = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 5));
    append_emitted(&response, &mut journal);
    let ended = send(&mut session, Event::Boundary { turn }).unwrap();
    append_emitted(&ended, &mut journal);
    let after = send(&mut session, inferred_event(1000)).unwrap();
    append_emitted(&after, &mut journal);
    let total = journal.iter().find_map(|record| match record {
        Record::TurnEnd {
            turn: ended, usage, ..
        } if *ended == turn => *usage,
        _ => None,
    });
    assert_eq!(total.map(|usage| usage.input_tokens), Some(16));
    same_replayed_state(&session, &journal);
}

#[test]
fn inferred_usage_overflow_is_rejected_without_mutating_totals() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let inferred = send(
        &mut session,
        Event::Inferred {
            at: stamp(),
            who: crate::Owner::Core,
            purpose: crate::InferredPurpose::Synthetic {
                id: "test/model".into(),
            },
            usage: Usage {
                input_tokens: u64::MAX,
                ..usage(0)
            },
        },
    )
    .unwrap();
    append_emitted(&inferred, &mut journal);
    assert!(
        send(&mut session, inferred_event(1)).is_err(),
        "overflowing usage is rejected before it changes the fold"
    );
    let response = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 0));
    append_emitted(&response, &mut journal);
    let ended = send(&mut session, Event::Boundary { turn }).unwrap();
    append_emitted(&ended, &mut journal);
    let total = journal.iter().find_map(|record| match record {
        Record::TurnEnd {
            turn: ended, usage, ..
        } if *ended == turn => *usage,
        _ => None,
    });
    assert_eq!(total.map(|usage| usage.input_tokens), Some(u64::MAX));
    same_replayed_state(&session, &journal);
}

#[test]
fn turn_totals_merge_changes_and_track_known_flags() {
    let mut totals = super::types::TurnTotals::default();
    assert_eq!(totals.usage(), None);
    assert!(totals.changes().is_empty());
    totals.add_usage(usage(10)).unwrap();
    totals
        .add_usage(Usage {
            input_tokens: 5,
            cached_input_tokens: 2,
            output_tokens: 3,
            reasoning_tokens: Some(4),
            cache_write_tokens: 1,
            cost_usd: Some(0.5),
        })
        .unwrap();
    assert_eq!(
        totals.usage(),
        Some(Usage {
            input_tokens: 15,
            cached_input_tokens: 2,
            output_tokens: 4,
            reasoning_tokens: None,
            cache_write_tokens: 1,
            cost_usd: None,
        })
    );
    let mut known = super::types::TurnTotals::default();
    known
        .add_usage(Usage {
            input_tokens: 1,
            cached_input_tokens: 1,
            output_tokens: 1,
            reasoning_tokens: Some(4),
            cache_write_tokens: 1,
            cost_usd: Some(0.5),
        })
        .unwrap();
    known
        .add_usage(Usage {
            input_tokens: 1,
            cached_input_tokens: 1,
            output_tokens: 1,
            reasoning_tokens: Some(6),
            cache_write_tokens: 1,
            cost_usd: Some(0.25),
        })
        .unwrap();
    assert_eq!(
        known.usage(),
        Some(Usage {
            input_tokens: 2,
            cached_input_tokens: 2,
            output_tokens: 2,
            reasoning_tokens: Some(10),
            cache_write_tokens: 2,
            cost_usd: Some(0.75),
        })
    );
    totals
        .add_changes(&[FileChange {
            path: "a".into(),
            added: 3,
            removed: 1,
        }])
        .unwrap();
    totals
        .add_changes(&[
            FileChange {
                path: "b".into(),
                added: 1,
                removed: 0,
            },
            FileChange {
                path: "a".into(),
                added: 2,
                removed: 2,
            },
        ])
        .unwrap();
    assert_eq!(
        totals.changes(),
        vec![
            FileChange {
                path: "a".into(),
                added: 5,
                removed: 3,
            },
            FileChange {
                path: "b".into(),
                added: 1,
                removed: 0,
            },
        ]
    );
}

#[test]
fn crash_repair_writes_accumulated_totals() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let streamed = stream_result(
        &mut session,
        turn,
        inference(Stop::EndTurn, &[("call", "read_file")], 10),
    );
    append_emitted(&streamed, &mut journal);
    let (_, effects) = Session::replay(journal.iter().cloned(), stamp()).unwrap();
    let mut repair = Vec::new();
    for effect in &effects {
        if let Effect::Emit(emit) = effect {
            repair.extend(emit.records.iter().cloned());
        }
    }
    let end = repair
        .iter()
        .find_map(|record| match record {
            Record::TurnEnd {
                turn: ended,
                usage,
                stop: TurnEndStop::Aborted,
                ..
            } if *ended == turn => Some(usage),
            _ => None,
        })
        .expect("repair ends the open turn");
    assert_eq!(end, &Some(usage(10)));
}
