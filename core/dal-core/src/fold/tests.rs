use super::helpers::{characters_billed_as, part_to_journal, truncated_args};
use super::types::MAX_STEERS;

use super::*;
use crate::journal::AssistantRoute;
use crate::update::ResolutionCause;
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

fn workspace_root() -> crate::workspace::Workspace {
    #[cfg(unix)]
    let root = PathBuf::from("/");
    #[cfg(windows)]
    let root = PathBuf::from("C:\\");
    crate::workspace::Workspace::new(root).unwrap()
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
        provider: None,
    }
}

fn usage(tokens: u64) -> Usage {
    Usage {
        input_tokens: tokens,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
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
    assert_eq!(replayed.rule_facts, live.rule_facts);
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
            elapsed_ms: None,
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

fn job_updates(effects: &[Effect]) -> Vec<&UpdateKind> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Emit(emit) => Some(emit.updates.iter()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[test]
fn running_jobs_publish_start_and_settle_once_and_compaction_stays_private() {
    let mut session = session();
    let exec = JobId::parse("01890f47-36b0-7cc4-8000-000000000011").unwrap();
    let started = Event::JobStarted {
        job: exec,
        kind: JobKind::Exec,
    };
    let out = send(&mut session, started.clone()).unwrap();
    assert!(matches!(
        job_updates(&out)[..],
        [UpdateKind::JobStarted { job }] if *job == exec
    ));
    let repeat = send(&mut session, started).unwrap();
    assert!(
        job_updates(&repeat).is_empty(),
        "a repeated start is silent"
    );
    let settled = Event::JobSettled {
        job: exec,
        outcome: JobOutcome::Exited { code: 0 },
    };
    let out = send(&mut session, settled.clone()).unwrap();
    assert!(matches!(
        job_updates(&out)[..],
        [UpdateKind::JobSettled { job }] if *job == exec
    ));
    let repeat = send(&mut session, settled).unwrap();
    assert!(
        job_updates(&repeat).is_empty(),
        "an unknown settle is silent"
    );

    let compaction = JobId::parse("01890f47-36b0-7cc4-8000-000000000012").unwrap();
    let out = send(
        &mut session,
        Event::JobStarted {
            job: compaction,
            kind: JobKind::Compaction,
        },
    )
    .unwrap();
    assert!(
        job_updates(&out).is_empty(),
        "compaction is a phase, not a job"
    );
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
            turn: None,
        },
    ));
    session.preflight_session_grant(request).unwrap();
    let mut emit = Emit::default();
    session
        .grant_resolved(
            request,
            &Answer::ApproveForSession,
            Some(client()),
            false,
            ResolutionCause::Answered,
            stamp(),
            &mut emit,
        )
        .unwrap();
    assert!(
        session
            .allow_always()
            .contains(&Name::parse_mapped_tool(tool).unwrap())
    );
    assert!(matches!(
        emit.records.as_slice(),
        [
            Record::Resolved {
                answer: Answer::ApproveForSession,
                ..
            },
            Record::AllowAlways { tool: recorded, .. }
        ] if **recorded == *tool
    ));
}

fn open_request(turn: Option<TurnId>, question: Question) -> Request {
    Request {
        id: RequestId::new_v7(),
        turn,
        owner: crate::Owner::Core,
        question,
        timeout: std::time::Duration::from_secs(30),
        default: Answer::Decline,
    }
}

#[test]
fn turn_cancel_keeps_turnless_questions_waiting() {
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
    let owned = open_request(
        Some(turn),
        Question::Confirm {
            text: "Proceed?".into(),
        },
    );
    let turnless = open_request(
        None,
        Question::Grant {
            ext: "web".into(),
            origin: "bundled".into(),
            capabilities: vec!["net".into()],
            detail: None,
        },
    );
    send(
        &mut session,
        Event::RequestOpened {
            request: owned.clone(),
        },
    )
    .unwrap();
    send(
        &mut session,
        Event::RequestOpened {
            request: turnless.clone(),
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
    let mut journal = Vec::new();
    append_emitted(&out, &mut journal);
    assert!(
        journal.iter().any(|record| matches!(
            record,
            Record::Resolved { request, answer: Answer::Cancel, .. } if *request == owned.id
        )),
        "the ending turn's question resolves as Cancel"
    );
    assert!(
        !journal.iter().any(|record| matches!(
            record,
            Record::Resolved { request, .. } if *request == turnless.id
        )),
        "the turnless grant question gains no terminal record"
    );
    assert_eq!(
        session
            .open_questions
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![turnless.id],
        "the turnless question survives the turn close"
    );
    assert!(matches!(session.phase(), Phase::Idle));
    let out = send(
        &mut session,
        Event::GrantResolved {
            request: turnless.id,
            answer: Answer::Approve,
            by: Some(client()),
            was_default: false,
            cause: ResolutionCause::Answered,
        },
    )
    .unwrap();
    let mut journal = Vec::new();
    append_emitted(&out, &mut journal);
    assert!(
        journal.iter().any(|record| matches!(
            record,
            Record::Resolved { request, answer: Answer::Approve, .. } if *request == turnless.id
        )),
        "the surviving turnless question still resolves by answer"
    );
    assert_eq!(session.open_questions, []);
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
    assert!(
        matches!(
            send(
                &mut session,
                Event::Steer {
                    turn: id(9),
                    text: "late".into()
                }
            ),
            Err(Rejection::WrongTurn { .. })
        ),
        "turn validation precedes capacity: a foreign turn is not SteerFull"
    );
}

#[test]
fn adjacent_text_deltas_journal_as_one_block() {
    let mut session = session();
    let turn = begin(&mut session);
    let delta = |text: &str| StreamEvent::Delta {
        channel: StreamChannel::Text,
        text: text.into(),
    };
    let response = Inference {
        events: vec![
            delta("Hello "),
            delta("streamed "),
            delta("world."),
            StreamEvent::Usage(usage(10)),
            StreamEvent::Stop(Stop::EndTurn),
        ],
    };
    let out = stream_result(&mut session, turn, response);
    let mut records = Vec::new();
    append_emitted(&out, &mut records);
    let content = records
        .iter()
        .find_map(|record| match record {
            Record::Assistant(entry) => match &entry.kind {
                EntryKind::Assistant { content, .. } => Some(content.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("the stream journals one assistant entry");
    assert_eq!(
        content,
        vec![Block::Text {
            text: "Hello streamed world.".into(),
            replay: None,
        }]
    );
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
        [Effect::Reply(Ok(Reply::Queued { turn: None }))]
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

fn follow_up(turn: TurnId, text: &str) -> Event {
    Event::Command {
        cmd: Command::FollowUp {
            turn,
            content: vec![Part::Text { text: text.into() }],
        },
        by: client(),
    }
}

fn cancel_queued(turn: TurnId) -> Event {
    Event::Command {
        cmd: Command::CancelQueued { turn },
        by: client(),
    }
}

fn queued_turn(effects: &[Effect]) -> TurnId {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Reply(Ok(Reply::Queued { turn })) => *turn,
            _ => None,
        })
        .expect("a follow-up reply names its turn")
}

fn notices(effects: &[Effect]) -> Vec<(&str, &str)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Emit(emit) => Some(&emit.updates),
            _ => None,
        })
        .flatten()
        .filter_map(|update| match update {
            UpdateKind::Notice(notice) => Some((notice.kind.as_ref(), notice.text.as_ref())),
            _ => None,
        })
        .collect()
}

#[test]
fn queued_follow_up_reply_names_its_turn_and_a_steer_reply_does_not() {
    let mut session = session();
    let turn = begin(&mut session);
    let queued = send(&mut session, follow_up(turn, "next")).unwrap();
    assert_eq!(queued_turn(&queued), id(2));
    let steered = send(
        &mut session,
        Event::Steer {
            turn,
            text: "now".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        steered.as_slice(),
        [Effect::Reply(Ok(Reply::Queued { turn: None }))]
    ));
}

#[test]
fn cancel_queued_removes_only_the_named_follow_up() {
    let mut session = session();
    let turn = begin(&mut session);
    let first = queued_turn(&send(&mut session, follow_up(turn, "first")).unwrap());
    let second = queued_turn(&send(&mut session, follow_up(turn, "second")).unwrap());
    assert_eq!(session.follow_ups_queued(), 2);

    let out = send(&mut session, cancel_queued(first)).unwrap();

    assert!(
        out.iter()
            .any(|effect| matches!(effect, Effect::Reply(Ok(Reply::Done(Output::Nothing)))))
    );
    assert_eq!(notices(&out), [("discarded", "first")]);
    assert_eq!(session.follow_ups_queued(), 1);
    assert!(matches!(session.phase(), Phase::Running { turn: active, .. } if *active == turn));

    stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 10));
    send(&mut session, Event::Boundary { turn }).unwrap();
    assert!(
        matches!(session.phase(), Phase::Settling { follow_up: Some((next, _)), .. } if *next == second),
        "the surviving follow-up starts next: {:?}",
        session.phase()
    );
}

#[test]
fn cancel_queued_rejects_an_id_that_is_not_queued() {
    let mut session = session();
    let turn = begin(&mut session);
    let queued = queued_turn(&send(&mut session, follow_up(turn, "only")).unwrap());
    send(&mut session, cancel_queued(queued)).unwrap();

    let Err(Rejection::Invalid { reason }) = send(&mut session, cancel_queued(queued)) else {
        panic!("a consumed queue id must be rejected");
    };
    assert_eq!(
        reason.as_ref(),
        "turn 2 has no queued follow-up. If the follow-up already started, cancel its turn instead."
    );
    assert!(matches!(
        send(&mut session, cancel_queued(id(99))),
        Err(Rejection::Invalid { .. })
    ));
    assert!(
        matches!(
            send(&mut session, cancel_queued(turn)),
            Err(Rejection::Invalid { .. })
        ),
        "the running turn is not a queued follow-up"
    );
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
fn stream_reminder_is_deferred_until_response_settlement() {
    let mut session = session();
    let turn = begin(&mut session);
    let queued = send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "retry-control".into(),
            text: "stream reminder".into(),
        },
    )
    .unwrap();
    assert_eq!(queued, [] as [Effect; 0]);

    let effects = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 0));
    let emit = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Emit(emit) => Some(emit),
            _ => None,
        })
        .unwrap();
    assert!(emit.records.iter().any(|record| matches!(
        record,
        Record::RuleFired { rule, turn: fired_turn, .. }
            if rule.as_ref() == "retry-control" && *fired_turn == turn
    )));
    assert_eq!(session.rule_seeds().len(), 1);
}

#[test]
fn stream_reminder_contributes_after_delivery() {
    let mut session = session();
    let turn = begin(&mut session);
    let before = session.projected_bytes;
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "retry-control".into(),
            text: "stream-only reminder".repeat(2048).into(),
        },
    )
    .unwrap();
    assert_eq!(session.projected_bytes, before);
    stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 0));
    assert!(session.projected_bytes > before);
}

#[test]
fn stream_retry_reuses_attempt_without_legacy_interrupt_cap() {
    let mut session = session();
    let turn = begin(&mut session);
    for _ in 0..5 {
        let effects = send(
            &mut session,
            Event::StreamRetry {
                turn,
                reminder: None,
            },
        )
        .unwrap();
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Infer(_)))
        );
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::Emit(_)))
        );
    }
}

#[test]
fn stream_retry_orders_queued_and_retry_reminders() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "queued".into(),
            text: "queued text".into(),
        },
    )
    .unwrap();
    let effects = send(
        &mut session,
        Event::StreamRetry {
            turn,
            reminder: Some(("retry".into(), "retry text".into())),
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
    let rules: Vec<&str> = emit
        .records
        .iter()
        .filter_map(|record| match record {
            Record::RuleFired { rule, .. } => Some(rule.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(rules, ["queued", "retry"]);
}

#[test]
fn cancelled_stream_discards_queued_reminders_and_stale_retry() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "late".into(),
            text: "late text".into(),
        },
    )
    .unwrap();
    let effects = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(inference(Stop::Cancelled, &[], 0)),
            partial: None,
        },
    )
    .unwrap();
    assert!(!effects.iter().any(|effect| matches!(
        effect,
        Effect::Emit(emit)
            if emit.records.iter().any(|record| matches!(record, Record::RuleFired { .. }))
    )));
    assert_eq!(session.rule_seeds(), []);
    assert_eq!(
        send(
            &mut session,
            Event::StreamRetry {
                turn,
                reminder: Some(("late".into(), "late text".into())),
            },
        )
        .unwrap(),
        [] as [Effect; 0]
    );
}
#[test]
fn stream_end_reserves_response_and_pending_entries() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "capacity".into(),
            text: "capacity reminder".into(),
        },
    )
    .unwrap();
    let leaf = session.tree.leaf;
    session.next_entry = None;
    let error = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(inference(Stop::EndTurn, &[], 0)),
            partial: None,
        },
    )
    .expect_err("stream end must reserve both response and deferred entries");
    assert!(matches!(error, Rejection::Invalid { .. }));
    assert_eq!(session.tree.leaf, leaf);
    assert_eq!(session.turn_flags.pending_reminders.len(), 1);
}
#[test]
fn stream_end_reserves_partial_tool_results() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "capacity".into(),
            text: "capacity reminder".into(),
        },
    )
    .unwrap();
    let partial = PartialResponse {
        content: vec![Block::ToolCall {
            id: CallId::new("partial-capacity"),
            name: "read_file".into(),
            input: RawJson::parse("{}").unwrap(),
            replay: None,
        }],
        usage: usage(0),
    };
    let leaf = session.tree.leaf;
    session.next_entry = Some(entry(u64::MAX - 1));
    let error = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Err(InferFailure::Fatal {
                message: "failed".into(),
                fix: None,
            }),
            partial: Some(partial),
        },
    )
    .expect_err("partial tool results must be reserved before folding");
    assert!(matches!(error, Rejection::Invalid { .. }));
    assert_eq!(session.tree.leaf, leaf);
    assert_eq!(session.turn_flags.pending_reminders.len(), 1);
}

#[test]
fn stream_end_reserves_failed_calls_for_each_stop() {
    for stop in [
        Some(Stop::Length),
        Some(Stop::Filter),
        Some(Stop::Failed),
        Some(Stop::MaxSteps),
        Some(Stop::Cancelled),
        None,
    ] {
        let mut session = session();
        let turn = begin(&mut session);
        let mut response = inference(
            stop.unwrap_or(Stop::EndTurn),
            &[("capacity", "read_file"), ("capacity", "read_file")],
            7,
        );
        if stop.is_none() {
            response.events.pop();
        }
        let rejected = Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(response.clone()),
            partial: None,
        };
        session.next_entry = Some(entry(u64::MAX));
        let tree = session.tree.clone();
        let phase = session.phase.clone();
        let totals = session.turn_totals.clone();
        let next_entry = session.next_entry;
        assert!(matches!(
            send(&mut session, rejected),
            Err(Rejection::Invalid { .. })
        ));
        assert_eq!(
            session.tree, tree,
            "rejected stop {stop:?} changed the tree"
        );
        assert_eq!(session.phase, phase);
        assert_eq!(session.turn_totals, totals);
        assert_eq!(session.next_entry, next_entry);
        session.next_entry = Some(entry(u64::MAX - 1));
        let effects = stream_result(&mut session, turn, response);
        let mut records = Vec::new();
        append_emitted(&effects, &mut records);
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record, Record::ToolResult(_)))
                .count(),
            1,
            "duplicate call identifiers need one failed result"
        );
    }
}

#[test]
fn cancel_partial_without_a_route_is_refused_before_reminders_clear() {
    let mut session = session();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "route".into(),
            text: "route reminder".into(),
        },
    )
    .unwrap();
    assert_eq!(session.active_model(), None, "no request has started yet");
    assert_eq!(session.turn_flags.pending_reminders.len(), 1);
    let before = session.clone();
    let refused = send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: Some(PartialResponse {
                content: vec![Block::Text {
                    text: "cut off".into(),
                    replay: None,
                }],
                usage: usage(1),
            }),
        },
    );
    assert!(matches!(refused, Err(Rejection::Invalid { .. })));
    assert_eq!(session, before, "a refused cancel dropped the reminders");
}

#[test]
fn cancel_reserves_every_failed_call_result_before_changing_anything() {
    let calls = [("first", "read_file"), ("second", "read_file")];
    // The cancel event and the cancel command end the turn the same way.
    let cancel = |turn, form: usize| match form {
        0 => Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: None,
        },
        _ => Event::Command {
            cmd: Command::Cancel {
                scope: CancelScope::Turn(turn),
            },
            by: client(),
        },
    };
    for form in 0..2 {
        let mut session = session();
        let turn = begin(&mut session);
        stream_result(&mut session, turn, inference(Stop::EndTurn, &calls, 1));
        // One free entry id for two failed results.
        session.next_entry = Some(entry(u64::MAX));
        let before = session.clone();
        let refused = send(&mut session, cancel(turn, form));
        assert!(matches!(refused, Err(Rejection::Invalid { .. })));
        assert_eq!(
            session, before,
            "refused cancel form {form} journaled a result"
        );

        // One free entry id for one failed result is enough.
        let mut session = self::session();
        let turn = begin(&mut session);
        stream_result(&mut session, turn, inference(Stop::EndTurn, &calls[..1], 1));
        session.next_entry = Some(entry(u64::MAX));
        send(&mut session, cancel(turn, form)).unwrap();
        assert!(matches!(session.phase(), Phase::Idle));
    }
}

/// A turn whose only call is dispatched and started, so its settlement is the
/// last one and runs the boundary.
fn with_one_started_call() -> (Session, TurnId) {
    let mut session = session();
    let turn = begin(&mut session);
    stream_result(
        &mut session,
        turn,
        inference(Stop::EndTurn, &[("only", "read_file")], 1),
    );
    let resolved = resolved_call("only", "read_file", false, Ok(ToolClass::Read));
    send(
        &mut session,
        Event::Resolved {
            turn,
            calls: vec![resolved],
            answerer_attached: false,
        },
    )
    .unwrap();
    send(
        &mut session,
        Event::CallStarted {
            turn,
            call: CallId::new("only"),
        },
    )
    .unwrap();
    (session, turn)
}

#[test]
fn the_last_settling_call_reserves_the_boundary_entries_it_runs() {
    let settle = |turn| Event::Settled {
        turn,
        call: CallId::new("only"),
        outcome: SettledOutcome::Ok {
            text: "ok".into(),
            data: None,
        },
        elapsed_ms: None,
    };
    // A queued steer is journaled at the boundary: two entries, one id left.
    let (mut session, turn) = with_one_started_call();
    send(
        &mut session,
        Event::Steer {
            turn,
            text: "steer".into(),
        },
    )
    .unwrap();
    session.next_entry = Some(entry(u64::MAX));
    let before = session.clone();
    let refused = send(&mut session, settle(turn));
    assert!(matches!(refused, Err(Rejection::Invalid { .. })));
    assert_eq!(session, before, "a refused settle journaled its result");

    // With nothing queued the result is the only entry, and the last id fits it.
    let (mut session, turn) = with_one_started_call();
    session.next_entry = Some(entry(u64::MAX));
    send(&mut session, settle(turn)).unwrap();
}

#[test]
fn resolved_reserves_every_failed_call_result_before_changing_anything() {
    let mut session = session();
    let turn = begin(&mut session);
    let calls = [("a", "read_file"), ("b", "read_file")];
    stream_result(&mut session, turn, inference(Stop::EndTurn, &calls, 1));
    // One free entry id for two failed results.
    session.next_entry = Some(entry(u64::MAX));
    let before = session.clone();
    let failed = |call| resolved_call(call, "read_file", false, Err(ResolveError::Unknown));
    let refused = send(
        &mut session,
        Event::Resolved {
            turn,
            calls: vec![failed("a"), failed("b")],
            answerer_attached: false,
        },
    );
    assert!(matches!(refused, Err(Rejection::Invalid { .. })));
    assert_eq!(session, before, "a refused resolve journaled one result");
}

#[test]
fn stream_end_reserves_truncation_boundary_inputs() {
    let mut session = session();
    let turn = begin(&mut session);
    session
        .queued_inputs
        .push(super::types::QueuedInput::Steer(vec![Part::Text {
            text: "steer".into(),
        }]));
    session.turn_flags.pending_suppressed = vec!["first".into(), "second".into()];
    session.next_entry = Some(entry(u64::MAX - 2));
    let tree = session.tree.clone();
    let queued = session.queued_inputs.clone();
    let response = inference(Stop::Length, &[("truncated", "read_file")], 0);
    assert!(matches!(
        send(
            &mut session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(response.clone()),
                partial: None,
            },
        ),
        Err(Rejection::Invalid { .. })
    ));
    assert_eq!(session.tree, tree);
    assert_eq!(session.queued_inputs, queued);
    assert_eq!(session.turn_flags.pending_suppressed.len(), 2);
    session.next_entry = Some(entry(u64::MAX - 4));
    let effects = stream_result(&mut session, turn, response);
    let mut records = Vec::new();
    append_emitted(&effects, &mut records);
    let kinds: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            Record::Assistant(_) => Some("assistant"),
            Record::ToolResult(_) => Some("result"),
            Record::User(_) => Some("steer"),
            Record::Reminder(_) => Some("suppressed"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        ["assistant", "result", "steer", "suppressed", "suppressed"]
    );
}

#[test]
fn overflow_compaction_discards_pending_reminders() {
    let mut session = session();
    send(&mut session, limits(0)).unwrap();
    let turn = begin(&mut session);
    send(
        &mut session,
        Event::StreamReminder {
            turn,
            rule: "overflow".into(),
            text: "discarded".into(),
        },
    )
    .unwrap();
    let effects = send(
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
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Compact {
            turn: Some(active), ..
        } if *active == turn
    )));
    assert_eq!(session.rule_seeds(), []);
}

#[test]
fn replay_retains_rule_facts_across_leaf_switches() {
    let user = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    };
    let user_id = user.id;
    let reminder = |id: EntryId, text: &str| {
        Record::Reminder(Entry {
            id,
            parent: Some(user_id),
            at: stamp(),
            kind: EntryKind::Reminder {
                source: "rule:gate".into(),
                text: text.into(),
            },
        })
    };
    let mut session = Session::replay_declared([
        Record::User(user),
        reminder(entry(2), "first"),
        Record::RuleFired {
            at: stamp(),
            turn: id(1),
            rule: "first".into(),
            entry: entry(2),
        },
        reminder(entry(3), "second"),
        Record::RuleFired {
            at: stamp(),
            turn: id(2),
            rule: "second".into(),
            entry: entry(3),
        },
    ])
    .unwrap();
    assert_eq!(session.rule_seeds().len(), 2);
    send(
        &mut session,
        Event::Command {
            cmd: Command::MoveLeaf(entry(2)),
            by: client(),
        },
    )
    .unwrap();
    assert_eq!(session.rule_seeds()[0].rule.as_ref(), "first");
    assert_eq!(session.rule_seeds()[1].rule.as_ref(), "second");
    send(
        &mut session,
        Event::Command {
            cmd: Command::MoveLeaf(entry(3)),
            by: client(),
        },
    )
    .unwrap();
    assert_eq!(session.rule_seeds().len(), 2);
}

#[test]
fn extension_rows_follow_their_explicit_reminder_anchors() {
    let user = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    };
    let mut session = Session::replay_declared([
        Record::User(user),
        Record::Reminder(Entry {
            id: entry(2),
            parent: Some(entry(1)),
            at: stamp(),
            kind: EntryKind::Reminder {
                source: "rule:first".into(),
                text: "first".into(),
            },
        }),
        Record::Reminder(Entry {
            id: entry(3),
            parent: Some(entry(1)),
            at: stamp(),
            kind: EntryKind::Reminder {
                source: "rule:second".into(),
                text: "second".into(),
            },
        }),
    ])
    .unwrap();
    session.fold_ext_at(
        Some(entry(2)),
        "ttsr",
        "gate",
        &RawJson::parse(r#"{"rule":"first"}"#).unwrap(),
    );
    session.fold_ext_at(
        Some(entry(3)),
        "ttsr",
        "gate",
        &RawJson::parse(r#"{"rule":"second"}"#).unwrap(),
    );
    send(
        &mut session,
        Event::Command {
            cmd: Command::MoveLeaf(entry(2)),
            by: client(),
        },
    )
    .unwrap();
    let visible: Vec<&str> = session.leaf_ext().map(|row| row.body.as_str()).collect();
    assert_eq!(visible, [r#"{"rule":"first"}"#]);
}

#[test]
fn projection_counts_context_reminders() {
    let user = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    };
    let text = "retained context ".repeat(2048);
    let equivalent = Session::replay_declared([
        Record::User(user.clone()),
        Record::User(Entry {
            id: entry(2),
            parent: Some(user.id),
            at: stamp(),
            kind: EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: text.clone().into(),
                }],
            },
        }),
    ])
    .unwrap();
    for source in ["rule:retry-control", crate::BEFORE_TURN_SOURCE] {
        let replayed = Session::replay_declared([
            Record::User(user.clone()),
            Record::Reminder(Entry {
                id: entry(2),
                parent: Some(user.id),
                at: stamp(),
                kind: EntryKind::Reminder {
                    source: source.into(),
                    text: text.clone().into(),
                },
            }),
        ])
        .unwrap();
        assert_eq!(
            replayed.tokens_since_last_compaction(),
            equivalent.tokens_since_last_compaction(),
            "{source}"
        );
    }
}

#[test]
fn context_measurement_tracks_selected_model_and_family() {
    let user = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    };
    let fallback = Session::replay_declared([Record::User(user.clone())])
        .unwrap()
        .tokens_since_last_compaction();
    let mut records = vec![
        Record::User(user.clone()),
        Record::Assistant(Entry {
            id: entry(2),
            parent: Some(user.id),
            at: stamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test-model".into(),
                route: Some(AssistantRoute::Api(Family::Chat)),
                content: Vec::new(),
                usage: usage(5500),
                stop: AssistantStop::Done,
                replay: None,
            },
        }),
    ];
    let mut live = Session::replay_declared(records.iter().cloned()).unwrap();
    for (family, model, expected) in [
        (Family::Chat, "other-model", fallback),
        (Family::Responses, "test-model", fallback),
        (Family::Chat, "test-model", 5500),
    ] {
        let out = send(
            &mut live,
            Event::Command {
                cmd: Command::SetModel {
                    model: ModelRoute::Api {
                        family,
                        model: model.into(),
                        provider: None,
                    },
                    save: crate::Save::SessionOnly,
                },
                by: client(),
            },
        )
        .unwrap();
        append_emitted(&out, &mut records);
        let replayed = Session::replay_declared(records.iter().cloned()).unwrap();
        assert_eq!(
            (
                live.tokens_since_last_compaction(),
                replayed.tokens_since_last_compaction(),
            ),
            (expected, expected),
            "{family:?}/{model}",
        );
    }
}

#[test]
fn replay_repairs_aborted_turn() {
    let session_id = crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap();
    let call = CallId::new("call");
    let header = crate::journal::Header {
        id: session_id,
        at: stamp(),
        workspace: workspace_root(),
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
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::ToolCall {
                id: call.clone(),
                name: "read_file".into(),
                input: RawJson::parse("{}").unwrap(),
                replay: None,
            }],
            usage: usage(1),
            stop: AssistantStop::ToolUse,
            replay: None,
        },
    };
    let records = [
        Record::Session(header),
        Record::TurnReserved {
            at: stamp(),
            turn: id(1),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(2),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(3),
        },
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

/// `replay_declared` must NOT run crash-recovery synthesis: the same open
/// turn stays open so journal inspection attributes state to the written
/// records only.
#[test]
fn replay_declared_keeps_the_open_turn_unrepaired() {
    let session_id = crate::id::SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").unwrap();
    let header = crate::journal::Header {
        id: session_id,
        at: stamp(),
        workspace: workspace_root(),
        product: crate::journal::Product::Dal,
        from: None,
    };
    let call = CallId::new("call");
    let assistant = Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test".into(),
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::ToolCall {
                id: call.clone(),
                name: "read_file".into(),
                input: RawJson::parse("{}").unwrap(),
                replay: None,
            }],
            usage: usage(1),
            stop: AssistantStop::ToolUse,
            replay: None,
        },
    };
    let records = [
        Record::Session(header),
        Record::TurnReserved {
            at: stamp(),
            turn: id(1),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(2),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(3),
        },
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
    let declared = Session::replay_declared(records).unwrap();
    // Replay mints a fresh boot generation and repair ToolResult entries;
    // the declared fold must mint neither.
    assert_eq!(declared.generation, None);
    let repairs = declared
        .tree
        .entries
        .values()
        .filter(|entry| matches!(entry.kind, EntryKind::ToolResult { .. }))
        .count();
    assert_eq!(repairs, 0, "no repair entries in the declared fold");
    // A journal ending inside a turn declares the live fold's shape —
    // `Running` at the boundary step — not `Idle`.
    assert!(
        matches!(declared.phase(), Phase::Running { turn, .. } if *turn == id(3)),
        "an unterminated turn must surface as Running: {:?}",
        declared.phase()
    );
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
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: calls
                .iter()
                .map(|call| Block::ToolCall {
                    id: CallId::new(*call),
                    name: "read_file".into(),
                    input: RawJson::parse("{}").unwrap(),
                    replay: None,
                })
                .collect(),
            usage: usage(1),
            stop: AssistantStop::ToolUse,
            replay: None,
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
            elapsed_ms: None,
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
        assert_eq!(results_of(&journal, call), []);

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
                elapsed_ms: None,
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
                    elapsed_ms: None,
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
        assert_eq!(session.promoted(), &BTreeSet::new());
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
            elapsed_ms: None,
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
        Record::TurnReserved {
            at: stamp(),
            turn: id(1),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(2),
        },
        Record::TurnReserved {
            at: stamp(),
            turn: id(3),
        },
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
    assert_eq!(results_of(&repair.records, "failed"), []);
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
    assert_eq!(replayed.promoted(), &BTreeSet::new());
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
    assert_eq!(results_of(&second.records, "call"), []);
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
        workspace: workspace_root(),
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
                elapsed_ms: None,
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

fn projection_journal(first_kept: Option<EntryId>) -> Vec<Record> {
    let user = |id, parent, text: Box<str>| {
        Record::User(Entry {
            id,
            parent,
            at: stamp(),
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text }],
            },
        })
    };
    vec![
        user(entry(1), None, "dropped ".repeat(1_000).into()),
        user(entry(2), Some(entry(1)), "abandoned ".repeat(500).into()),
        Record::Leaf {
            at: stamp(),
            to: Some(entry(1)),
        },
        user(entry(3), Some(entry(1)), "kept context".into()),
        Record::Compaction(Entry {
            id: entry(4),
            parent: Some(entry(3)),
            at: stamp(),
            kind: EntryKind::Compaction {
                summary: Some("superseded summary ".repeat(100).into()),
                first_kept,
                tokens_before: 10_000,
                replay: Some(RawJson::parse(r#"{"private":"compactor state"}"#).unwrap()),
                usage: None,
                parts: vec![JournalPart::Text {
                    text: "replacement".into(),
                }],
                parts_tokens: 3,
            },
        }),
    ]
}

#[test]
fn projection_matches_replay_after_branch_selection() {
    let mut records = projection_journal(Some(entry(3)));
    records.push(Record::Assistant(Entry {
        id: entry(5),
        parent: Some(entry(4)),
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: Vec::new(),
            usage: usage(90_000),
            stop: AssistantStop::Done,
            replay: None,
        },
    }));
    let mut live = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    for leaf in [entry(4), entry(1), entry(3), entry(5), entry(4)] {
        let out = send(
            &mut live,
            Event::Command {
                cmd: Command::MoveLeaf(leaf),
                by: client(),
            },
        )
        .unwrap();
        append_emitted(&out, &mut records);
        let replayed = Session::replay_declared(records.iter().cloned()).unwrap();
        assert_eq!(
            (
                live.tokens_since_last_compaction(),
                live.compactions(),
                live.cache_key(),
            ),
            (
                replayed.tokens_since_last_compaction(),
                replayed.compactions(),
                replayed.cache_key(),
            ),
            "selected leaf {leaf}",
        );
    }
}

#[test]
fn projection_excludes_replaced_prefix_and_compactor_metadata() {
    let compacted = Session::replay_declared(projection_journal(None)).unwrap();
    assert_eq!(
        compacted.tokens_since_last_compaction(),
        3,
        "only the replacement counts, at its stored bill: not the replaced prefix, the \
         superseded summary, or the replay payload"
    );
}

#[test]
fn projection_includes_first_kept_prefix_and_later_entries() {
    let later = Entry {
        id: entry(5),
        parent: Some(entry(4)),
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "later question".into(),
            }],
        },
    };
    let mut records = projection_journal(Some(entry(3)));
    records.push(Record::User(later));
    let compacted = Session::replay_declared(records).unwrap();
    let kept_and_later = u64::try_from("kept context".len() + "later question".len()).unwrap();
    assert_eq!(
        compacted.tokens_since_last_compaction(),
        crate::tokens::estimate_tokens(kept_and_later.saturating_add(characters_billed_as(3))),
        "the replacement at its stored bill, then the kept prefix and later entries as text"
    );
}

/// A one-page replacement: a label and a stored PNG of `png_bytes` bytes.
fn png_page(png_bytes: u64) -> Vec<JournalPart> {
    vec![
        JournalPart::Text {
            text: "letter://history/1.1".into(),
        },
        JournalPart::ImageBlob {
            mime: "image/png".into(),
            blob: "0".repeat(64).into(),
            bytes: png_bytes,
        },
    ]
}

/// A session whose only live context is a compaction that replaced the whole
/// path with `parts`, billed at `parts_tokens`, in a 100 000 token window
/// that compacts at 85 000 tokens. No usage is measured, so the context size
/// is the unmeasured estimate.
fn session_with_parts_compaction(parts: Vec<JournalPart>, parts_tokens: u64) -> Session {
    let records = [
        Record::User(Entry {
            id: entry(1),
            parent: None,
            at: stamp(),
            kind: EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "question".into(),
                }],
            },
        }),
        Record::Compaction(Entry {
            id: entry(2),
            parent: Some(entry(1)),
            at: stamp(),
            kind: EntryKind::Compaction {
                summary: None,
                first_kept: None,
                tokens_before: 40_000,
                replay: None,
                usage: None,
                parts,
                parts_tokens,
            },
        }),
    ];
    let mut session = Session::replay(records, stamp()).unwrap().0;
    send(&mut session, limits(0)).unwrap();
    session
}

#[test]
fn characters_billed_as_is_the_exact_inverse_of_the_token_estimate() {
    for tokens in [0, 1, 2, 3, 4, 5, 6, 7, 2_996, 85_000, u64::from(u32::MAX)] {
        assert_eq!(
            crate::tokens::estimate_tokens(characters_billed_as(tokens)),
            tokens,
            "{tokens} tokens"
        );
    }
    assert_eq!(characters_billed_as(u64::MAX), u64::MAX / 2, "saturates");
}

#[test]
fn a_parts_compaction_is_estimated_at_its_bill_not_at_its_image_bytes() {
    // Three million bytes of PNG, billed as one image of 3 000 tokens. Read as
    // prose those bytes are 857 143 estimated tokens, ten times the window.
    let session = session_with_parts_compaction(png_page(3_000_000), 3_000);
    assert_eq!(session.measured_context_tokens(), None);
    assert_eq!(session.tokens_since_last_compaction(), 3_000);
    assert_eq!(
        session.should_compact(),
        None,
        "3 000 billed tokens in a 100 000 token window are far under the 85 000 threshold"
    );
}

#[test]
fn a_parts_compaction_triggers_when_its_bill_reaches_the_threshold() {
    let under = session_with_parts_compaction(png_page(1), 80_000);
    assert_eq!(under.tokens_since_last_compaction(), 80_000);
    assert_eq!(under.should_compact(), None);

    let over = session_with_parts_compaction(png_page(1), 90_000);
    assert_eq!(over.tokens_since_last_compaction(), 90_000);
    assert_eq!(
        over.should_compact(),
        Some(CompactionReason::Threshold),
        "the bill alone crosses the threshold, however few bytes the images hold"
    );
}

#[test]
fn a_zero_bill_weighs_zero_as_it_does_at_the_cut_point() {
    let session = session_with_parts_compaction(png_page(3_000_000), 0);
    assert_eq!(session.tokens_since_last_compaction(), 0);
    assert_eq!(session.should_compact(), None);
}

#[test]
fn an_unbounded_bill_saturates_instead_of_wrapping() {
    let session = session_with_parts_compaction(png_page(1), u64::MAX);
    assert!(session.tokens_since_last_compaction() > 1 << 60);
    assert_eq!(session.should_compact(), Some(CompactionReason::Threshold));
}

#[test]
fn a_live_parts_compaction_is_estimated_like_its_replay() {
    let mut records = vec![Record::User(Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    })];
    let mut live = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    send(&mut live, limits(0)).unwrap();
    let out = send(
        &mut live,
        Event::Command {
            cmd: Command::Compact { focus: None },
            by: client(),
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
    let settled = CompactionSummary {
        compactor: name("history"),
        tokens_before: 40_000,
        tokens_after: 3_100,
        summary: None,
        first_kept: Some(entry(1)),
        replay: None,
        usage: None,
        parts: png_page(3_000_000),
        parts_tokens: 3_000,
        letters: Vec::new(),
    };
    for event in [
        Event::JobStarted {
            job,
            kind: JobKind::Compaction,
        },
        Event::CompactionSettled {
            turn: None,
            outcome: Ok(settled),
        },
        Event::JobSettled {
            job,
            outcome: JobOutcome::Exited { code: 0 },
        },
    ] {
        let out = send(&mut live, event).unwrap();
        append_emitted(&out, &mut records);
    }
    let replayed = Session::replay_declared(records).unwrap();
    let expected = crate::tokens::estimate_tokens(
        u64::try_from("question".len())
            .unwrap()
            .saturating_add(characters_billed_as(3_000)),
    );
    assert_eq!(
        (
            live.tokens_since_last_compaction(),
            replayed.tokens_since_last_compaction()
        ),
        (expected, expected),
        "the kept question as text plus the replacement at its bill"
    );
    assert_eq!(live.should_compact(), None);
}

#[test]
fn projection_tracks_latest_compaction_on_selected_branch() {
    let mut records = projection_journal(Some(entry(3)));
    records.extend([
        Record::Assistant(Entry {
            id: entry(5),
            parent: Some(entry(4)),
            at: stamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test-model".into(),
                route: Some(AssistantRoute::Api(Family::Chat)),
                content: Vec::new(),
                usage: usage(90_000),
                stop: AssistantStop::Done,
                replay: None,
            },
        }),
        Record::Compaction(Entry {
            id: entry(6),
            parent: Some(entry(5)),
            at: stamp(),
            kind: EntryKind::Compaction {
                summary: Some("latest summary".into()),
                first_kept: None,
                tokens_before: 90_000,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        }),
    ]);
    let equivalent = Session::replay_declared([Record::User(Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "latest summary".into(),
            }],
        },
    })])
    .unwrap();
    let mut live = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    let latest_key = live.cache_key();
    for (leaf, expected_usage, count) in [
        (entry(6), equivalent.tokens_since_last_compaction(), 2),
        (entry(5), 90_000, 1),
        (entry(6), equivalent.tokens_since_last_compaction(), 2),
    ] {
        let out = send(
            &mut live,
            Event::Command {
                cmd: Command::MoveLeaf(leaf),
                by: client(),
            },
        )
        .unwrap();
        append_emitted(&out, &mut records);
        let replayed = Session::replay_declared(records.iter().cloned()).unwrap();
        assert_eq!(
            (live.tokens_since_last_compaction(), live.compactions()),
            (expected_usage, count),
        );
        assert_eq!(
            (
                replayed.tokens_since_last_compaction(),
                replayed.compactions()
            ),
            (expected_usage, count),
        );
        assert_eq!(live.cache_key(), replayed.cache_key());
        assert_eq!(live.cache_key() == latest_key, count == 2);
    }
}

#[test]
fn projection_recalculates_after_live_compaction() {
    let mut records = projection_journal(Some(entry(3)));
    records.pop().unwrap();
    let mut live = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    send(&mut live, limits(0)).unwrap();
    let out = send(
        &mut live,
        Event::Command {
            cmd: Command::Compact { focus: None },
            by: client(),
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
    let out = send(
        &mut live,
        Event::JobStarted {
            job,
            kind: JobKind::Compaction,
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let out = send(
        &mut live,
        Event::CompactionSettled {
            turn: None,
            outcome: Ok(CompactionSummary {
                first_kept: Some(entry(3)),
                ..compact_summary(10_000, 10)
            }),
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let out = send(
        &mut live,
        Event::JobSettled {
            job,
            outcome: JobOutcome::Exited { code: 0 },
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let replayed = Session::replay_declared(records).unwrap();
    let equivalent = Session::replay_declared([Record::User(Entry {
        id: entry(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "kept contextkept context".into(),
            }],
        },
    })])
    .unwrap();
    assert_eq!(
        (
            live.tokens_since_last_compaction(),
            replayed.tokens_since_last_compaction()
        ),
        (
            equivalent.tokens_since_last_compaction(),
            equivalent.tokens_since_last_compaction()
        ),
    );
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
                replay: None,
            },
            Block::ToolCall {
                id: call.clone(),
                name: "read_file".into(),
                input: RawJson::parse("{}").unwrap(),
                replay: None,
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
    assert_eq!(late, []);
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
fn replayed_jobs_follow_journal_records_not_replay_state() {
    let settled = JobId::parse("01890f47-36b0-7cc4-8000-000000000002").unwrap();
    let live_at_close = JobId::parse("01890f47-36b0-7cc4-8000-000000000003").unwrap();
    let records = [
        Record::Job {
            at: stamp(),
            job: settled,
            event: JobEvent::Started {
                kind: Some("exec".into()),
            },
        },
        Record::Job {
            at: stamp(),
            job: settled,
            event: JobEvent::Settled { outcome: None },
        },
        // The recorded reminder consumed the settled job in the live fold;
        // replaying it must drain the pending entry the same way.
        Record::Reminder(Entry {
            id: entry(1),
            parent: None,
            at: stamp(),
            kind: EntryKind::Reminder {
                source: "jobs.finished".into(),
                text: "jobs.finished: one".into(),
            },
        }),
        Record::Job {
            at: stamp(),
            job: live_at_close,
            event: JobEvent::Started {
                kind: Some("exec".into()),
            },
        },
    ];
    let (session, _) = Session::replay(records, stamp()).unwrap();
    // The orphan synthesized at finish settles the still-open job: nothing
    // stays live, and the pending reminder queue holds only that loss.
    assert_eq!(session.live_jobs.as_slice(), []);
    assert_eq!(
        session.ended_jobs.as_slice(),
        [(live_at_close, JobOutcome::Lost)]
    );
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
            proptest::prop_assert_eq!(duplicate, []);
        }

        for call in &names {
            let settled = send(&mut session, Event::Settled {
                turn,
                call: CallId::new(call.as_str()),
                outcome: SettledOutcome::Ok { text: "result".into(), data: None },
                elapsed_ms: None,
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
fn before_turn_text_journals_its_own_reminder_after_user_entry() {
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
    let [
        Record::TurnStart { turn: started, .. },
        Record::User(user),
        Record::Reminder(reminder),
    ] = emit.records.as_slice()
    else {
        panic!(
            "opening journals turn start, user, reminder: {:?}",
            emit.records
        );
    };
    assert_eq!(*started, turn);
    let EntryKind::User { parts } = &user.kind else {
        panic!("user record holds a user entry");
    };
    assert!(matches!(
        parts.as_slice(),
        [JournalPart::Text { text }] if text.as_ref() == "question"
    ));
    assert_eq!(reminder.parent, Some(user.id));
    assert!(matches!(
        &reminder.kind,
        EntryKind::Reminder { source, text }
        if source.as_ref() == crate::BEFORE_TURN_SOURCE && text.as_ref() == "first\nsecond"
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
fn before_turn_text_without_a_free_entry_id_is_rejected_before_the_phase_moves() {
    let mut session = session();
    send(&mut session, prompt("question")).unwrap();
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    session.next_entry = None;
    let guard = |text: Option<&str>| Event::Guard {
        turn,
        call: None,
        extension: None,
        outcome: HookOutcome::new(
            HookEvent::BeforeTurn,
            HookVerdict::BeforeTurn(text.map(Into::into)),
        )
        .unwrap(),
    };
    assert!(matches!(
        send(&mut session, guard(Some("hint"))),
        Err(Rejection::Invalid { .. })
    ));
    assert!(matches!(session.phase(), Phase::Opening { .. }));
    send(&mut session, guard(None)).unwrap();
    assert!(matches!(session.phase(), Phase::Running { .. }));
}

#[test]
fn settled_elapsed_reaches_the_journal_the_update_and_the_replayed_view() {
    let mut session = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut session, &mut journal);
    let streamed = stream_result(
        &mut session,
        turn,
        inference(Stop::EndTurn, &[("call", "read_file")], 10),
    );
    append_emitted(&streamed, &mut journal);
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
                text: "read".into(),
                data: None,
            },
            elapsed_ms: Some(42),
        },
    )
    .unwrap();
    append_emitted(&settled, &mut journal);

    assert!(matches!(
        &only_tool_result(&journal).kind,
        EntryKind::ToolResult {
            elapsed_ms: Some(42),
            ..
        }
    ));
    let outcome = settled
        .iter()
        .filter_map(|effect| match effect {
            Effect::Emit(emit) => Some(emit),
            _ => None,
        })
        .flat_map(|emit| emit.updates.iter())
        .find_map(|update| match update {
            UpdateKind::ToolSettled { outcome, .. } => Some(outcome),
            _ => None,
        })
        .expect("settling a call publishes its outcome");
    assert_eq!(outcome.elapsed_ms, Some(42));
    let replayed = Session::replay(journal.iter().cloned(), stamp()).unwrap().0;
    assert_eq!(replayed.tree, session.tree);
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
    assert_eq!(end.1.len(), 0);
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
            elapsed_ms: None,
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
            premium_requests: None,
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
    assert_eq!(totals.changes(), []);
    totals.add_usage(usage(10)).unwrap();
    totals
        .add_usage(Usage {
            input_tokens: 5,
            cached_input_tokens: 2,
            output_tokens: 3,
            reasoning_tokens: Some(4),
            cache_write_tokens: 1,
            premium_requests: Some(0.5),
            cost_usd: Some(0.5),
        })
        .unwrap();
    assert_eq!(
        totals.usage(),
        Some(Usage {
            input_tokens: 15,
            cached_input_tokens: 2,
            output_tokens: 4,
            reasoning_tokens: Some(4),
            cache_write_tokens: 1,
            premium_requests: Some(0.5),
            cost_usd: Some(0.5),
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
            premium_requests: Some(0.5),
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
            premium_requests: Some(0.25),
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
            premium_requests: Some(0.75),
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
fn turn_premium_measured_zero_stays_known_while_missing_is_unknown() {
    let measured = |premium| Usage {
        premium_requests: Some(premium),
        ..usage(1)
    };
    let mut totals = super::types::TurnTotals::default();
    totals.add_usage(measured(0.0)).unwrap();
    totals.add_usage(measured(0.0)).unwrap();
    assert_eq!(
        totals.usage().and_then(|usage| usage.premium_requests),
        Some(0.0)
    );
    totals.add_usage(measured(1.5)).unwrap();
    assert_eq!(
        totals.usage().and_then(|usage| usage.premium_requests),
        Some(1.5)
    );
    totals.add_usage(usage(1)).unwrap();
    assert_eq!(
        totals.usage().and_then(|usage| usage.premium_requests),
        None
    );
}

#[test]
fn turn_premium_rejects_invalid_and_overflowing_measurements() {
    for premium in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut totals = super::types::TurnTotals::default();
        let rejected = totals.add_usage(Usage {
            premium_requests: Some(premium),
            ..usage(1)
        });
        assert!(
            matches!(rejected, Err(Rejection::Invalid { .. })),
            "premium {premium:?} is rejected"
        );
        assert_eq!(
            totals.usage(),
            None,
            "a rejected premium measurement folds none of its members"
        );
    }
    let mut totals = super::types::TurnTotals::default();
    totals
        .add_usage(Usage {
            premium_requests: Some(f64::MAX),
            ..usage(1)
        })
        .unwrap();
    let before = totals.clone();
    let overflowing = Usage {
        premium_requests: Some(f64::MAX),
        ..usage(1)
    };
    assert!(
        totals.check_usage(overflowing).is_err(),
        "the preflight check refuses the same premium sum"
    );
    assert!(
        totals.add_usage(overflowing).is_err(),
        "a premium sum that stops being finite is rejected"
    );
    assert_eq!(
        totals, before,
        "a refused premium sum leaves the token members it passed unfolded"
    );
}

#[test]
fn zero_usage_carries_no_premium_measurement() {
    assert_eq!(super::helpers::zero_usage().premium_requests, None);
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

#[test]
fn replay_rejects_out_of_order_turn_ids() {
    // The live fold allocates turn ids strictly increasing, so a journal that
    // restarts an older id is torn or forged.
    assert!(contradicts(vec![
        turn_start_record(3),
        turn_end_record(3),
        turn_start_record(2),
        turn_end_record(2),
    ]));
}

#[test]
fn tokens_since_compaction_estimate_rounds_up_at_three_and_a_half_characters_per_token() {
    // One token per 3.5 characters, rounded up: ceil(2n / 7). The expected
    // counts are the literal rate, never the estimator under test; each pair
    // of neighbours straddles a rounding boundary.
    for (characters, tokens) in [
        (1_usize, 1_u64),
        (7, 2),
        (8, 3),
        (34, 10),
        (35, 10),
        (36, 11),
        (70, 20),
        (71, 21),
    ] {
        let mut session = session();
        send(
            &mut session,
            Event::Command {
                cmd: Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text {
                        text: "a".repeat(characters).into(),
                    }],
                },
                by: client(),
            },
        )
        .unwrap();
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        send(&mut session, guard(turn)).unwrap();
        assert_eq!(
            session.tokens_since_last_compaction(),
            tokens,
            "{characters} characters"
        );
    }
}

#[test]
fn overflow_failures_mark_turn_end_overflowed() {
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
    send(
        &mut session,
        Event::CompactionSettled {
            turn: Some(turn),
            outcome: Ok(compact_summary(100, 50)),
        },
    )
    .unwrap();
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
    assert!(
        failed.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                overflowed: true,
                ..
            }
        )),
        "a second overflow in one round ends the turn overflowed"
    );
}

#[test]
fn a_failure_that_is_not_overflow_leaves_the_mark_off() {
    let mut session = session();
    send(&mut session, limits(0)).unwrap();
    let turn = begin(&mut session);
    let failed = send(
        &mut session,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Err(InferFailure::Fatal {
                message: "refused".into(),
                fix: None,
            }),
            partial: None,
        },
    )
    .unwrap();
    assert!(
        failed.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                overflowed: false,
                ..
            }
        )),
        "a plain failure is not an overflow"
    );
}

#[test]
fn an_overflow_without_a_compactor_still_marks_the_turn() {
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
    let failed = send(
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
    assert!(
        failed.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                overflowed: true,
                ..
            }
        )),
        "an overflow with no compactor ends the turn overflowed"
    );
}

#[test]
fn an_overflow_failure_journals_the_same_line_as_any_other_failure() {
    let mut session = session();
    send(&mut session, limits(0)).unwrap();
    let turn = begin(&mut session);
    let overflow = |message: &str| Event::StreamEnded {
        turn,
        model: route(),
        family: Family::Chat,
        result: Err(InferFailure::Overflow {
            code: "context".into(),
            message: message.into(),
        }),
        partial: None,
    };
    send(&mut session, overflow("too large")).unwrap();
    send(
        &mut session,
        Event::CompactionSettled {
            turn: Some(turn),
            outcome: Ok(compact_summary(100, 50)),
        },
    )
    .unwrap();
    let failed = send(&mut session, overflow("still too large")).unwrap();
    let mut journal = Vec::new();
    append_emitted(&failed, &mut journal);
    let Some(record @ Record::TurnEnd { .. }) = journal.last() else {
        panic!("the failed turn wrote no turn_end record: {journal:?}");
    };
    let line = String::from_utf8(crate::journal::encode(record).unwrap()).unwrap();
    assert!(
        line.contains(
            "\"stop\":{\"failed\":\"Context overflow recovery failed: still too large\"},"
        ),
        "the overflow mark never reaches the journal: {line}"
    );
    assert!(!line.contains("overflow\":"), "{line}");
}

#[test]
fn a_failed_overflow_compaction_marks_the_turn_overflowed() {
    let mut session = session();
    send(&mut session, limits(0)).unwrap();
    let turn = begin(&mut session);
    send(
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
    let failed = send(
        &mut session,
        Event::CompactionSettled {
            turn: Some(turn),
            outcome: Err("compactor failed".into()),
        },
    )
    .unwrap();
    assert!(
        failed.iter().any(|effect| matches!(
            effect,
            Effect::Stop {
                overflowed: true,
                ..
            }
        )),
        "a compaction that fails while recovering an overflow ends the turn overflowed"
    );
}

fn route_assistant(
    entry_id: u64,
    parent: Option<EntryId>,
    route: Option<AssistantRoute>,
    api: Family,
    model: &str,
    tokens: u64,
) -> Record {
    Record::Assistant(Entry {
        id: entry(entry_id),
        parent,
        at: stamp(),
        kind: EntryKind::Assistant {
            api,
            model: model.into(),
            route,
            content: Vec::new(),
            usage: usage(tokens),
            stop: AssistantStop::Done,
            replay: None,
        },
    })
}

fn user_question(entry_id: u64, parent: Option<EntryId>) -> Record {
    Record::User(Entry {
        id: entry(entry_id),
        parent,
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "question".into(),
            }],
        },
    })
}

fn select_model(session: &mut Session, route: &ModelRoute) {
    send(
        session,
        Event::Command {
            cmd: Command::SetModel {
                model: route.clone(),
                save: crate::Save::SessionOnly,
            },
            by: client(),
        },
    )
    .unwrap();
}

fn decode_journal(records: &[Record]) -> Vec<Record> {
    records
        .iter()
        .map(|record| {
            let line = crate::journal::encode(record).unwrap();
            crate::journal::decode(&line).unwrap().record
        })
        .collect()
}

#[test]
fn restore_uses_newest_provenance_among_colliding_spellings() {
    let spelling = "dalgon/normal";
    let records = [
        user_question(1, None),
        route_assistant(
            2,
            Some(entry(1)),
            Some(AssistantRoute::Api(Family::Chat)),
            Family::Chat,
            spelling,
            1_000,
        ),
        user_question(3, Some(entry(2))),
        route_assistant(
            4,
            Some(entry(3)),
            Some(AssistantRoute::Synthetic),
            Family::Chat,
            spelling,
            2_000,
        ),
        user_question(5, Some(entry(4))),
        route_assistant(
            6,
            Some(entry(5)),
            Some(AssistantRoute::Harness),
            Family::Chat,
            spelling,
            3_000,
        ),
    ];
    let records = decode_journal(&records);
    let api = ModelRoute::Api {
        family: Family::Chat,
        model: spelling.into(),
        provider: None,
    };
    let synthetic = ModelRoute::Synthetic {
        id: spelling.into(),
    };
    let harness = ModelRoute::Harness {
        id: spelling.into(),
    };
    let mut live = Session::replay_declared(records.iter().cloned()).unwrap();
    select_model(&mut live, &harness);
    assert_eq!(
        live.measured_context_tokens(),
        Some(3_000),
        "the newest assistant carries the measurement"
    );
    select_model(&mut live, &api);
    assert_eq!(
        live.measured_context_tokens(),
        None,
        "the same spelling under a different route kind must not measure"
    );
    select_model(&mut live, &synthetic);
    assert_eq!(
        live.measured_context_tokens(),
        None,
        "an older assistant must not be searched backward"
    );
    for (leaf, route, tokens) in [
        (entry(4), &synthetic, 2_000),
        (entry(2), &api, 1_000),
        (entry(6), &harness, 3_000),
    ] {
        send(
            &mut live,
            Event::Command {
                cmd: Command::MoveLeaf(leaf),
                by: client(),
            },
        )
        .unwrap();
        assert_eq!(
            live.last_response_model().as_ref(),
            Some(route),
            "startup identity must follow leaf {leaf}, not later journal entries"
        );
        select_model(&mut live, route);
        assert_eq!(
            live.measured_context_tokens(),
            Some(tokens),
            "leaf {leaf} must restore the provenance of its own branch"
        );
    }
    let mut replayed = Session::replay_declared(records.iter().cloned()).unwrap();
    select_model(&mut replayed, &harness);
    assert_eq!(
        replayed.measured_context_tokens(),
        live.measured_context_tokens()
    );
}

#[test]
fn restored_route_family_is_independent_of_wire_family() {
    let records = [
        user_question(1, None),
        route_assistant(
            2,
            Some(entry(1)),
            Some(AssistantRoute::Api(Family::Chat)),
            Family::Anthropic,
            "test-model",
            7_000,
        ),
    ];
    let records = decode_journal(&records);
    let chat = ModelRoute::Api {
        family: Family::Chat,
        model: "test-model".into(),
        provider: None,
    };
    let anthropic = ModelRoute::Api {
        family: Family::Anthropic,
        model: "test-model".into(),
        provider: None,
    };
    let mut live = Session::replay_declared(records.iter().cloned()).unwrap();
    select_model(&mut live, &chat);
    assert_eq!(
        live.measured_context_tokens(),
        Some(7_000),
        "the recorded route family decides, not the wire family"
    );
    select_model(&mut live, &anthropic);
    assert_eq!(
        live.measured_context_tokens(),
        None,
        "matching the wire family alone must not measure"
    );
}

#[test]
fn newer_assistant_without_provenance_invalidates_older_usage() {
    let records = [
        user_question(1, None),
        route_assistant(
            2,
            Some(entry(1)),
            Some(AssistantRoute::Api(Family::Chat)),
            Family::Chat,
            "test-model",
            1_000,
        ),
        user_question(3, Some(entry(2))),
        route_assistant(4, Some(entry(3)), None, Family::Chat, "test-model", 2_000),
    ];
    let records = decode_journal(&records);
    let mut live = Session::replay_declared(records.iter().cloned()).unwrap();
    select_model(
        &mut live,
        &ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
            provider: None,
        },
    );
    assert_eq!(live.measured_context_tokens(), None);
    assert_eq!(live.should_compact(), None);
}

#[test]
fn partial_assistant_measurement_matches_across_reopen() {
    let mut live = session();
    send(&mut live, limits(0)).unwrap();
    let mut records = Vec::new();
    let turn = open_turn(&mut live, &mut records);
    let out = send(
        &mut live,
        Event::RequestStarted {
            turn,
            model: route(),
            family: Family::Chat,
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let partial = PartialResponse {
        content: vec![Block::Text {
            text: "partial answer".into(),
            replay: None,
        }],
        usage: usage(90_000),
    };
    let out = send(
        &mut live,
        Event::Cancel {
            scope: CancelScope::Turn(turn),
            partial: Some(partial),
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let Some(Record::Assistant(recorded)) = records
        .iter()
        .find(|record| matches!(record, Record::Assistant(_)))
    else {
        panic!("the interrupted turn wrote no partial assistant: {records:?}");
    };
    assert_eq!(
        recorded.kind,
        EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::Text {
                text: "partial answer".into(),
                replay: None,
            }],
            usage: usage(90_000),
            stop: AssistantStop::Cancelled,
            replay: None,
        },
    );
    let records = decode_journal(&records);
    let mut replayed = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    send(&mut replayed, limits(0)).unwrap();
    assert_eq!(live.should_compact(), Some(CompactionReason::Threshold));
    assert_eq!(replayed.should_compact(), Some(CompactionReason::Threshold));
    assert_eq!(live.tokens_since_last_compaction(), 90_000);
    assert_eq!(replayed.tokens_since_last_compaction(), 90_000);
}

#[test]
fn missing_stop_partial_also_becomes_the_reopen_measurement() {
    let mut live = session();
    send(&mut live, limits(0)).unwrap();
    let mut records = Vec::new();
    let turn = open_turn(&mut live, &mut records);
    let out = send(
        &mut live,
        Event::RequestStarted {
            turn,
            model: route(),
            family: Family::Chat,
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    let out = send(
        &mut live,
        Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(Inference {
                events: vec![
                    StreamEvent::Delta {
                        channel: StreamChannel::Text,
                        text: "truncated answer".into(),
                    },
                    StreamEvent::Usage(usage(90_000)),
                ],
            }),
            partial: None,
        },
    )
    .unwrap();
    append_emitted(&out, &mut records);
    assert!(
        records
            .iter()
            .any(|record| matches!(record, Record::Assistant(_))),
        "the missing-stop turn journaled its partial assistant: {records:?}"
    );
    let records = decode_journal(&records);
    let mut replayed = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
    send(&mut replayed, limits(0)).unwrap();
    assert_eq!(live.tokens_since_last_compaction(), 90_000);
    assert_eq!(replayed.tokens_since_last_compaction(), 90_000);
    assert_eq!(replayed.should_compact(), live.should_compact());
}

#[test]
fn completed_routes_survive_reopen_across_all_kinds() {
    let cases = [
        (
            ModelRoute::Api {
                family: Family::Chat,
                model: "api-model".into(),
                provider: None,
            },
            AssistantRoute::Api(Family::Chat),
            6_000_u64,
        ),
        (
            ModelRoute::Synthetic {
                id: "acme/gpt-x".into(),
            },
            AssistantRoute::Synthetic,
            4_000,
        ),
        (
            ModelRoute::Harness {
                id: "dalgon/normal".into(),
            },
            AssistantRoute::Harness,
            5_000,
        ),
    ];
    for (route, expected_route, tokens) in cases {
        let mut live = session();
        let mut records = Vec::new();
        let turn = open_turn(&mut live, &mut records);
        let out = send(
            &mut live,
            Event::RequestStarted {
                turn,
                model: route.clone(),
                family: Family::Chat,
            },
        )
        .unwrap();
        append_emitted(&out, &mut records);
        let out = send(
            &mut live,
            Event::StreamEnded {
                turn,
                model: route.clone(),
                family: Family::Chat,
                result: Ok(inference(Stop::EndTurn, &[], tokens)),
                partial: None,
            },
        )
        .unwrap();
        append_emitted(&out, &mut records);
        let Some(Record::Assistant(recorded)) = records
            .iter()
            .find(|record| matches!(record, Record::Assistant(_)))
        else {
            panic!("the completed response wrote no assistant record: {records:?}");
        };
        match &recorded.kind {
            EntryKind::Assistant {
                route: Some(actual),
                ..
            } => assert_eq!(actual, &expected_route, "{route:?}"),
            other => panic!("completed responses journal their route: {other:?}"),
        }
        let records = decode_journal(&records);
        let mut replayed = Session::replay(records.iter().cloned(), stamp()).unwrap().0;
        select_model(&mut live, &route);
        select_model(&mut replayed, &route);
        assert_eq!(live.tokens_since_last_compaction(), tokens, "{route:?}");
        assert_eq!(replayed.tokens_since_last_compaction(), tokens, "{route:?}");
    }
}

#[test]
fn last_response_model_follows_the_leaf_chain() {
    let chained = Session::replay_declared([
        user_question(1, None),
        route_assistant(
            2,
            Some(entry(1)),
            Some(AssistantRoute::Api(Family::Chat)),
            Family::Chat,
            "test-model",
            1_000,
        ),
    ])
    .unwrap();
    assert_eq!(
        chained.last_response_model(),
        Some(ModelRoute::Api {
            family: Family::Chat,
            model: "test-model".into(),
            provider: None,
        }),
        "a complete user-to-assistant chain returns that assistant route"
    );
    let dangling = Session::replay_declared([user_question(2, Some(entry(1)))]).unwrap();
    assert_eq!(
        dangling.last_response_model(),
        None,
        "a user whose parent id is absent returns no route"
    );
    let non_assistant_parent =
        Session::replay_declared([user_question(1, None), user_question(2, Some(entry(1)))])
            .unwrap();
    assert_eq!(
        non_assistant_parent.last_response_model(),
        None,
        "a present non-assistant parent does not invent a route"
    );
}

/// Journal job records are a declared source too: `dev fold` and other
/// declared-state projections must see live and ended jobs, not only
/// update records. Reverting the mirror in `Replay::job` leaves both
/// ledgers empty.
#[test]
fn declared_fold_mirrors_job_records_into_session_jobs() {
    let at = stamp();
    let job = JobId::parse("01890f47-36b0-7cc4-8000-000000000003").unwrap();
    let mut fold = DeclaredFold::new();
    fold.push(&Record::Job {
        at,
        job,
        event: JobEvent::Started {
            kind: Some("exec".into()),
        },
    })
    .unwrap();
    assert!(matches!(
        fold.session().unwrap().live_jobs.as_slice(),
        [(id, Some(JobKind::Exec))] if *id == job
    ));
    fold.push(&Record::Job {
        at,
        job,
        event: JobEvent::Settled {
            outcome: Some(JobOutcome::Exited { code: 0 }),
        },
    })
    .unwrap();
    let session = fold.session().unwrap();
    assert_eq!(session.live_jobs.len(), 0);
    assert!(matches!(
        session.ended_jobs.as_slice(),
        [(id, JobOutcome::Exited { code: 0 })] if *id == job
    ));
}
#[cfg(test)]
mod threshold_estimate_fallback {
    //! The automatic-threshold trigger must estimate from the
    //! projected branch whenever measured usage is absent, stale (pre-dating a
    //! compaction), or tied to another model, using the shared token-estimation
    //! owner in `crate::tokens`. A change in the turn outcome (a `Compact`
    //! effect firing or not) is the observable, so deleting the fallback
    //! breaks these tests instead of just uninvoking a helper.
    use super::*;

    /// Drives a real prompt turn to the trigger point. `open_round` runs
    /// inside the guard event (`begin_turn` -> boundary -> `open_round`), so
    /// the guard's effects are the observable: `Effect::Compact` means the
    /// threshold fired.
    fn open_prompt_turn(session: &mut Session, text: &str) -> (TurnId, Vec<Effect>) {
        send(
            session,
            Event::Command {
                cmd: Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text { text: text.into() }],
                },
                by: client(),
            },
        )
        .unwrap();
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("prompt did not open a turn: {phase:?}"),
        };
        let out = send(session, guard(turn)).unwrap();
        (turn, out)
    }

    fn compacted(out: &[Effect]) -> bool {
        out.iter()
            .any(|effect| matches!(effect, Effect::Compact { .. }))
    }

    /// ~455 projected KiB -> estimate = ceil(2 * bytes / 7) ~= 149k tokens,
    /// above the 85% line of a 100k window with `min_tokens` = 1.
    fn heavy_text() -> String {
        "context ".repeat(65_000)
    }

    /// ~228 projected KiB -> estimate ~= 74k tokens: below the 85k line
    /// alone, past it when one more entry of the same size joins.
    fn medium_text() -> String {
        "context ".repeat(32_500)
    }

    #[test]
    fn unmeasured_projected_context_crossing_threshold_compacts() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        assert_eq!(session.last_usage, None, "precondition: no usage yet");
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(
            compacted(&out),
            "unmeasured context above the estimate threshold must compact"
        );
        assert!(session.pending_compaction.is_some());
    }

    #[test]
    fn unmeasured_context_below_threshold_does_not_compact() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let (_, out) = open_prompt_turn(&mut session, "tiny question");
        assert!(
            !compacted(&out),
            "a small unmeasured context must not trigger compaction"
        );
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn min_tokens_floor_gates_the_estimate() {
        let mut session = session();
        send(&mut session, limits(200_000)).unwrap();
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(
            !compacted(&out),
            "the estimate must respect limits.compact.min_tokens"
        );
    }

    #[test]
    fn model_changed_context_compacts_on_the_estimate() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let (turn, _) = open_prompt_turn(&mut session, "small question");
        let out = stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 500));
        assert!(
            !compacted(&out),
            "measured same-model usage below the threshold must not compact"
        );
        send(&mut session, Event::Boundary { turn }).unwrap();
        // Switch the requested model: the stored measurement now belongs to
        // another route, so only the estimate remains valid.
        select_model(
            &mut session,
            &ModelRoute::Api {
                family: Family::Anthropic,
                model: "other-model".into(),
                provider: None,
            },
        );
        // Grow the projected branch stream-free in sub-threshold steps
        // (200 000 bytes -> 57 143 estimated tokens each): the first
        // post-switch turn must not fire, the second crosses the 85k line
        // on the estimate alone.
        let mid_text = "context ".repeat(25_000);
        let (turn, out) = open_prompt_turn(&mut session, &mid_text);
        assert!(
            !compacted(&out),
            "the first sub-threshold estimate must not compact"
        );
        send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        let (_, out) = open_prompt_turn(&mut session, &mid_text);
        assert!(
            compacted(&out),
            "a stale measurement from another model must fall back to the estimate"
        );
    }

    #[test]
    fn measured_eligible_usage_still_takes_precedence() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let (turn, _) = open_prompt_turn(&mut session, "question");
        let out = one_read_round(&mut session, turn, "precedence", 90_000);
        // 90_000 measured tokens >= 85_000 with a tiny projection: the
        // measured value must decide, not the estimate.
        assert_eq!(session.tokens_since_last_compaction(), 90_000);
        assert!(
            compacted(&out),
            "eligible measured usage must still trigger compaction"
        );
    }

    #[test]
    fn zero_window_still_disables_the_trigger() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        send(
            &mut session,
            Event::Limits {
                window: 0,
                max_steps: 0,
                compact: CompactLimits {
                    threshold: 0.85,
                    min_tokens: 1,
                    keep_tokens: 20_000,
                    enabled: true,
                    compactor_available: true,
                },
            },
        )
        .unwrap();
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(!compacted(&out), "window == 0 must disable the trigger");
    }

    #[test]
    fn disabled_flag_still_suppresses_the_estimate_trigger() {
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
                    enabled: false,
                    compactor_available: true,
                },
            },
        )
        .unwrap();
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(!compacted(&out), "compact.enabled = false must suppress");
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn missing_compactor_keeps_the_estimate_only_notifying() {
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
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(!compacted(&out));
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
        assert!(
            out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_none")))),
            "an eligible trigger with no compactor must notify compaction_none"
        );
    }

    #[test]
    fn open_breaker_suppresses_the_estimate_trigger() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        session.breaker_open = true;
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(!compacted(&out), "an open breaker must suppress");
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn breaker_arms_when_estimate_driven_compactions_fail_three_times() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        // Stream-free: every measured_context_tokens() stays None, so each
        // round is judged by the estimate alone.
        for index in 0..3 {
            let (turn, out) = open_prompt_turn(&mut session, &heavy_text());
            assert!(
                compacted(&out),
                "estimate-driven compaction {index} must fire"
            );
            let out = send(
                &mut session,
                Event::CompactionSettled {
                    turn: Some(turn),
                    outcome: Err("compactor unavailable".into()),
                },
            )
            .unwrap();
            assert!(
                out.iter().any(|effect| matches!(effect, Effect::Emit(emit) if emit.updates.iter().any(|update| matches!(update, UpdateKind::Notice(notice) if notice.kind.as_ref() == "compaction_ended")))),
                "failed compaction {index} must emit compaction_ended"
            );
            if index < 2 {
                assert!(session.auto_compaction_on(), "breaker closed after {index}");
            }
            send(
                &mut session,
                Event::Cancel {
                    scope: CancelScope::Turn(turn),
                    partial: None,
                },
            )
            .unwrap();
        }
        assert!(
            !session.auto_compaction_on(),
            "breaker must arm on the third failure"
        );
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(
            !compacted(&out),
            "the open breaker must suppress estimate-driven compaction"
        );
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn resolved_compaction_consumes_the_estimate_so_no_loop_fires() {
        // Stream-free so measured_context_tokens() stays None. The context is
        // split across two ~260 KiB user entries (~74k estimated tokens
        // each): one alone stays under the 85k line, both together cross it,
        // and dropping the older one brings the projection back under.
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let (turn, out) = open_prompt_turn(&mut session, &medium_text());
        assert!(
            !compacted(&out),
            "a single sub-threshold entry must not compact"
        );
        send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        let (_, out) = open_prompt_turn(&mut session, &medium_text());
        assert!(
            compacted(&out),
            "two entries past the line must fire on the estimate"
        );
        let turn = match session.phase() {
            Phase::Running { turn, .. } => *turn,
            phase => panic!("compaction did not pause the turn: {phase:?}"),
        };
        // Keep only the newest user entry (first_kept = entry 2): the
        // projected bytes collapse back to ~74k estimated tokens, so the next
        // trigger point must infer, not re-compact.
        let mut summary = compact_summary(148_000, 74_000);
        summary.first_kept = Some(entry(2));
        send(
            &mut session,
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Ok(summary),
            },
        )
        .unwrap();
        assert_eq!(session.compactions(), 1);
        send(
            &mut session,
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        )
        .unwrap();
        let (_, out) = open_prompt_turn(&mut session, "next");
        assert!(
            !compacted(&out),
            "a resolved compaction must not re-trigger on its own summary"
        );
        assert!(out.iter().any(|effect| matches!(effect, Effect::Infer(_))));
    }

    #[test]
    fn stale_pre_compaction_usage_falls_back_to_the_estimate() {
        let mut session = session();
        send(&mut session, limits(1)).unwrap();
        let (turn, _) = open_prompt_turn(&mut session, "before compaction");
        stream_result(&mut session, turn, inference(Stop::EndTurn, &[], 500));
        send(&mut session, Event::Boundary { turn }).unwrap();
        send(
            &mut session,
            Event::Command {
                cmd: Command::Compact { focus: None },
                by: client(),
            },
        )
        .unwrap();
        assert!(matches!(session.phase(), Phase::Compacting { .. }));
        settle_manual_compaction(&mut session, 1_000, 100);
        assert_eq!(session.compactions(), 1);
        // The stored usage predates compaction 1: measured_context_tokens()
        // now returns None and the estimate must decide.
        assert_eq!(session.measured_context_tokens(), None);
        let (_, out) = open_prompt_turn(&mut session, &heavy_text());
        assert!(
            compacted(&out),
            "stale pre-compaction usage must fall back to the estimate"
        );
    }
}

// ---------------------------------------------------------------------------
// Private-effect fold: births, pairing, terminals, repair.
//
// Private turns are attribution only: no test below reserves or opens a
// turn, pinning that private records never consult the turn sequence.
// ---------------------------------------------------------------------------

fn private_owner(name: &str) -> crate::Owner {
    crate::Owner::Extension {
        name: name.into(),
        origin: "user".into(),
    }
}

fn private_birth(id: crate::PrivateLoopId, who: crate::Owner, turn: Option<TurnId>) -> Record {
    Record::PrivateLoop {
        at: stamp(),
        r#loop: id,
        who,
        turn,
    }
}

fn private_begin(
    id: crate::PrivateLoopId,
    token: crate::PrivateEffectId,
    who: crate::Owner,
    turn: Option<TurnId>,
    round: u32,
    call: &str,
    tool: &str,
) -> Record {
    Record::PrivateStart {
        at: stamp(),
        r#loop: id,
        token,
        who,
        turn,
        round,
        call: CallId::new(call),
        name: tool.into(),
        args: RawJson::parse(r#"{"x":1}"#).unwrap(),
    }
}

fn private_close(
    id: crate::PrivateLoopId,
    token: crate::PrivateEffectId,
    outcome: crate::PrivateOutcome,
) -> Record {
    Record::PrivateEnd {
        at: stamp(),
        r#loop: id,
        token,
        outcome,
    }
}

fn settled_outcome() -> crate::PrivateOutcome {
    crate::PrivateOutcome::Settled {
        is_error: false,
        parts: vec![JournalPart::Text { text: "ok".into() }],
    }
}

fn private_ends(records: &[Record]) -> Vec<(crate::PrivateLoopId, crate::PrivateEffectId)> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::PrivateEnd { r#loop, token, .. } => Some((*r#loop, *token)),
            _ => None,
        })
        .collect()
}

#[test]
fn private_transitions_contradict_exactly_once_per_shape() {
    let owner_a = private_owner("a");
    let owner_b = private_owner("b");
    let born = crate::PrivateLoopId::new_v7();
    let other = crate::PrivateLoopId::new_v7();
    let token = crate::PrivateEffectId::new_v7();

    // UnbornLoop: a start or an end with no prior birth, even childless.
    assert!(contradicts(vec![private_begin(
        born,
        token,
        owner_a.clone(),
        None,
        0,
        "c",
        "tool"
    )]));
    assert!(contradicts(vec![private_close(
        born,
        token,
        settled_outcome()
    )]));

    // LoopBornTwice: a childless duplicate birth contradicts ...
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_birth(born, owner_a.clone(), None),
    ]));
    // ... and so does a re-birth after the loop settled.
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_begin(born, token, owner_a.clone(), None, 0, "c", "tool"),
        private_close(born, token, settled_outcome()),
        private_birth(born, owner_a.clone(), None),
    ]));

    // EffectStartedTwice: one (loop, token) starts once.
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_begin(born, token, owner_a.clone(), None, 0, "c", "tool"),
        private_begin(born, token, owner_a.clone(), None, 1, "c", "tool"),
    ]));

    // LoopMismatch: the start must repeat the birth's owner and turn.
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_begin(born, token, owner_b.clone(), None, 0, "c", "tool"),
    ]));
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), Some(id(1))),
        private_begin(born, token, owner_a.clone(), Some(id(2)), 0, "c", "tool"),
    ]));

    // EndWithoutStart: the birth exists but no start does; the cross-loop
    // end (same token, other loop) finds no row either.
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_close(born, token, settled_outcome()),
    ]));
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_birth(other, owner_a.clone(), None),
        private_begin(born, token, owner_a.clone(), None, 0, "c", "tool"),
        private_close(other, token, settled_outcome()),
    ]));

    // EndedTwice: one (loop, token) ends once.
    assert!(contradicts(vec![
        private_birth(born, owner_a.clone(), None),
        private_begin(born, token, owner_a.clone(), None, 0, "c", "tool"),
        private_close(born, token, settled_outcome()),
        private_close(born, token, settled_outcome()),
    ]));

    // Control: the same wire call id under two loops settles disjointly.
    let second = crate::PrivateEffectId::new_v7();
    assert!(!contradicts(vec![
        private_birth(born, owner_a.clone(), Some(id(1))),
        private_birth(other, owner_a.clone(), Some(id(1))),
        private_begin(born, token, owner_a.clone(), Some(id(1)), 0, "c", "tool"),
        private_begin(other, second, owner_a.clone(), Some(id(1)), 0, "c", "tool"),
        private_close(born, token, settled_outcome()),
        private_close(other, second, settled_outcome()),
    ]));
}

#[test]
fn private_same_wire_call_across_loops_settles_in_end_order() {
    let owner = private_owner("a");
    let first = crate::PrivateLoopId::new_v7();
    let second_loop = crate::PrivateLoopId::new_v7();
    let first_token = crate::PrivateEffectId::new_v7();
    let second_token = crate::PrivateEffectId::new_v7();
    let journal = vec![
        private_birth(first, owner.clone(), Some(id(1))),
        private_birth(second_loop, owner.clone(), Some(id(1))),
        private_begin(
            first,
            first_token,
            owner.clone(),
            Some(id(1)),
            0,
            "c",
            "tool",
        ),
        private_begin(
            second_loop,
            second_token,
            owner.clone(),
            Some(id(1)),
            0,
            "c",
            "tool",
        ),
        // Ends arrive latest-loop-first; terminals follow end order.
        private_close(second_loop, second_token, settled_outcome()),
        private_close(first, first_token, settled_outcome()),
    ];
    let (session, _) = Session::replay(journal, stamp()).unwrap();
    assert_eq!(session.private_terminal_len(), 2);
    let terminals = session.private_terminals_since(0);
    assert_eq!(terminals[0].token, second_token);
    assert_eq!(terminals[1].token, first_token);
    assert_eq!(terminals[0].r#loop, second_loop);
    let facts: Vec<_> = session.private_facts().collect();
    assert!(facts.iter().all(|fact| fact.outcome.is_some()));
}

#[test]
fn private_repair_marks_each_unresolved_start_exactly_once() {
    let owner = private_owner("a");
    let first = crate::PrivateLoopId::new_v7();
    let second_loop = crate::PrivateLoopId::new_v7();
    let first_token = crate::PrivateEffectId::new_v7();
    let second_token = crate::PrivateEffectId::new_v7();
    let mut journal = vec![
        private_birth(first, owner.clone(), None),
        private_birth(second_loop, owner.clone(), None),
        private_begin(first, first_token, owner.clone(), None, 0, "c", "tool"),
        private_begin(
            second_loop,
            second_token,
            owner.clone(),
            None,
            3,
            "c",
            "tool",
        ),
        private_close(first, first_token, settled_outcome()),
    ];
    let (session, effects) = Session::replay(journal.clone(), stamp()).unwrap();
    // Exactly one repair end, in row order, for the unresolved start.
    let [Effect::Emit(repair)] = effects.as_slice() else {
        panic!("replay must return one repair batch")
    };
    let ends = private_ends(&repair.records);
    assert_eq!(ends, [(second_loop, second_token)]);
    assert!(matches!(
        &repair.records[0],
        Record::PrivateEnd {
            outcome: crate::PrivateOutcome::Indeterminate,
            ..
        }
    ));
    // The repair end is terminal like any other: the settled row first,
    // then the indeterminate repair, both in end journal order.
    let terminals = session.private_terminals_since(0);
    assert_eq!(terminals.len(), 2);
    assert!(matches!(
        terminals[0].outcome,
        crate::PrivateOutcome::Settled { .. }
    ));
    assert!(matches!(
        terminals[1].outcome,
        crate::PrivateOutcome::Indeterminate
    ));
    assert_eq!(
        (terminals[1].r#loop, terminals[1].token),
        (second_loop, second_token)
    );

    // A second open over the persisted repair mints nothing new.
    journal.extend(repair.records.iter().cloned());
    let (reopened, again) = Session::replay(journal, stamp()).unwrap();
    let [Effect::Emit(second)] = again.as_slice() else {
        panic!("replay must return one repair batch")
    };
    assert_eq!(private_ends(&second.records), []);
    assert_eq!(reopened.private_terminal_len(), 2);
}

#[test]
fn private_declared_replay_never_repairs() {
    let owner = private_owner("a");
    let id = crate::PrivateLoopId::new_v7();
    let token = crate::PrivateEffectId::new_v7();
    let session = Session::replay_declared(vec![
        private_birth(id, owner.clone(), None),
        private_begin(id, token, owner.clone(), None, 0, "c", "tool"),
    ])
    .unwrap();
    // Inspection sees the started row but no terminal and no repair.
    assert_eq!(session.private_terminal_len(), 0);
    assert_eq!(session.private_terminals_since(0), []);
    let facts: Vec<_> = session.private_facts().collect();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].outcome, None);
    assert_eq!((facts[0].r#loop, facts[0].token), (id, token));
}

#[test]
fn private_absent_records_leave_no_terminals() {
    // A journal that predates private records replays with an empty
    // terminal log ...
    let journal = vec![Record::Ext {
        at: stamp(),
        ext: "a".into(),
        kind: "k".into(),
        body: RawJson::parse(r"{}").unwrap(),
    }];
    let (session, _) = Session::replay(journal, stamp()).unwrap();
    assert_eq!(session.private_terminal_len(), 0);
    assert_eq!(session.private_terminals_since(0), []);

    // ... and so does a fork view, which carries entry-path records only:
    // branch() drops non-entry private records, so the clone starts empty
    // while the source keeps its audit.
    let owner = private_owner("a");
    let id = crate::PrivateLoopId::new_v7();
    let token = crate::PrivateEffectId::new_v7();
    let source = vec![
        private_birth(id, owner.clone(), None),
        private_begin(id, token, owner.clone(), None, 0, "c", "tool"),
        private_close(id, token, settled_outcome()),
    ];
    let (before, _) = Session::replay(source.clone(), stamp()).unwrap();
    assert_eq!(before.private_terminal_len(), 1);
    let fork_view: Vec<Record> = source
        .into_iter()
        .filter(|record| record.entry().is_some())
        .collect();
    let (forked, _) = Session::replay(fork_view, stamp()).unwrap();
    assert_eq!(forked.private_terminal_len(), 0);
}

#[test]
fn private_live_check_then_fold_publishes_once() {
    let mut live = session();
    let owner = private_owner("a");
    let id = crate::PrivateLoopId::new_v7();
    let token = crate::PrivateEffectId::new_v7();

    assert!(live.check_private_loop(id).is_ok());
    live.fold_private_loop(id, owner.clone(), None);
    assert!(live.check_private_start(id, token, &owner, None).is_ok());
    live.fold_private_start(id, token, &owner, None, 2, "tool");
    // The started row is in flight: invisible to publication.
    assert_eq!(live.private_terminal_len(), 0);
    assert_eq!(live.private_terminals_since(0), []);
    assert!(live.check_private_end(id, token).is_ok());
    live.fold_private_end(id, token, settled_outcome());
    assert_eq!(live.private_terminal_len(), 1);
    assert_eq!(live.private_terminals_since(0).len(), 1);
    assert_eq!(live.private_terminals_since(1), []);

    // A rejected write changes nothing.
    assert_eq!(
        live.check_private_loop(id),
        Err(crate::PrivateConflict::LoopBornTwice)
    );
    assert_eq!(live.private_terminal_len(), 1);
}

#[test]
#[should_panic(expected = "private publication cursor")]
fn private_terminal_cursor_beyond_len_fails_closed() {
    let live = session();
    let _ = live.private_terminals_since(1);
}

#[test]
fn types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}

    assert_send_sync::<Session>();
    assert_send_sync::<Event>();
    assert_send_sync::<Effect>();
    assert_send_sync::<ReplayError>();
    assert_send_sync::<ResolveError>();
    assert_send_sync::<PrivateConflict>();
    assert_send_sync::<crate::DenyReason>();
    assert_send_sync::<crate::ConfigError>();
    assert_send_sync::<crate::DecodeError>();
    assert_send_sync::<crate::EncodeError>();
    assert_send_sync::<crate::Rejection>();
}

/// Provider replay capture policy, driven through `Session::step`.
///
/// The fold decides once per completed response whether a provider capture
/// rides the closing assistant entry: a `Captured` payload is measured as the
/// exact journal line and journals only when that line fits `MAX_RECORD`; a
/// `Refused` capture (or an over-bound line) journals the content without a
/// payload and fails the turn once; a capture on a tool-call response is
/// ignored.
///
/// Duplicate and post-usage captures are `debug_assert!`ed protocol
/// violations (`helpers.rs`), not recoverable input. The policy test choice
/// (GAP-3c): debug builds prove the assertion fires (`#[should_panic]`);
/// builds without debug assertions prove the documented release behavior
/// (first capture wins, a capture after the usage is dropped). Each cfg
/// compiles only its own half, so a default `cargo test` exercises the debug
/// half and a profile with `debug-assertions = false` exercises the other.
mod capture_policy {
    use super::*;
    use crate::fold::helpers::assistant_stop;
    use crate::journal::{MAX_RECORD, decode, encode};
    use crate::model::{ReplayCapture, ReplayRefusal};

    const BODY: &str = "Texte diffuse.";
    const STOPS: [Stop; 6] = [
        Stop::EndTurn,
        Stop::Length,
        Stop::Filter,
        Stop::MaxSteps,
        Stop::Cancelled,
        Stop::Failed,
    ];
    /// Every refusal an adapter can report; `LineBound` is the fold's own.
    const ADAPTER_REFUSALS: [ReplayRefusal; 7] = [
        ReplayRefusal::RecordNotUtf8,
        ReplayRefusal::RecordNotRoundtrip,
        ReplayRefusal::RecordNotJson,
        ReplayRefusal::UserRecordNotFound,
        ReplayRefusal::NoRecords,
        ReplayRefusal::CheckpointNotCoveringTurn,
        ReplayRefusal::RouteMixed,
    ];

    fn text() -> StreamEvent {
        StreamEvent::Delta {
            channel: StreamChannel::Text,
            text: BODY.into(),
        }
    }

    fn captured(payload: &str) -> StreamEvent {
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Captured {
                payload: RawJson::parse(payload).unwrap(),
            },
        }
    }

    fn refused(reason: ReplayRefusal) -> StreamEvent {
        StreamEvent::MessageReplay {
            capture: ReplayCapture::Refused { reason },
        }
    }

    fn tool_call() -> StreamEvent {
        StreamEvent::ToolCall {
            call: CallId::new("call-1"),
            name: "read_file".into(),
            args: RawJson::parse("{}").unwrap(),
        }
    }

    /// The body text, `between` events, then the usage and the stop: the
    /// capture sits inside the grammar's window, before the usage.
    fn closing(between: Vec<StreamEvent>, stop: Stop) -> Inference {
        let mut events = vec![text()];
        events.extend(between);
        events.push(StreamEvent::Usage(usage(10)));
        events.push(StreamEvent::Stop(stop));
        Inference { events }
    }

    /// A response whose capture arrives after its usage: outside the window.
    fn late(capture: StreamEvent) -> Inference {
        Inference {
            events: vec![
                text(),
                StreamEvent::Usage(usage(10)),
                capture,
                StreamEvent::Stop(Stop::EndTurn),
            ],
        }
    }

    /// One response whose capture is a JSON string of `chars` characters.
    fn padded(chars: usize) -> Inference {
        let mut json = String::with_capacity(chars + 2);
        json.push('"');
        json.push_str(&"a".repeat(chars));
        json.push('"');
        closing(vec![captured(&json)], Stop::EndTurn)
    }

    fn run(response: Inference) -> (Session, TurnId, Vec<Effect>) {
        let mut session = session();
        let turn = begin(&mut session);
        let out = stream_result(&mut session, turn, response);
        (session, turn, out)
    }

    fn records(out: &[Effect]) -> impl Iterator<Item = &Record> {
        out.iter()
            .filter_map(|effect| match effect {
                Effect::Emit(emit) => Some(emit.records.iter()),
                _ => None,
            })
            .flatten()
    }

    fn only_assistant(out: &[Effect]) -> (&Record, &EntryKind) {
        let mut found = records(out).filter_map(|record| match record {
            Record::Assistant(entry) => Some((record, &entry.kind)),
            _ => None,
        });
        let first = found
            .next()
            .expect("the response journals an assistant entry");
        assert!(
            found.next().is_none(),
            "the response journals exactly one assistant entry"
        );
        first
    }

    fn turn_ends(out: &[Effect]) -> Vec<&TurnEndStop> {
        records(out)
            .filter_map(|record| match record {
                Record::TurnEnd { stop, .. } => Some(stop),
                _ => None,
            })
            .collect()
    }

    fn stop_effects(out: &[Effect]) -> Vec<Stop> {
        out.iter()
            .filter_map(|effect| match effect {
                Effect::Stop { stop, .. } => Some(*stop),
                _ => None,
            })
            .collect()
    }

    fn ended_updates(out: &[Effect]) -> Vec<Stop> {
        out.iter()
            .filter_map(|effect| match effect {
                Effect::Emit(emit) => Some(emit.updates.iter()),
                _ => None,
            })
            .flatten()
            .filter_map(|update| match update {
                UpdateKind::TurnEnded { stop, .. } => Some(*stop),
                _ => None,
            })
            .collect()
    }

    fn replay_of(kind: &EntryKind) -> Option<&RawJson> {
        match kind {
            EntryKind::Assistant { replay, .. } => replay.as_ref(),
            other => panic!("expected an assistant entry kind: {other:?}"),
        }
    }

    fn content_of(kind: &EntryKind) -> &[Block] {
        match kind {
            EntryKind::Assistant { content, .. } => content,
            other => panic!("expected an assistant entry kind: {other:?}"),
        }
    }

    fn body() -> [Block; 1] {
        [Block::Text {
            text: BODY.into(),
            replay: None,
        }]
    }

    fn failed_token(stop: &TurnEndStop) -> &str {
        match stop {
            TurnEndStop::Failed { message } => message,
            other => panic!("expected a failed turn end, got {other:?}"),
        }
    }

    /// No emitted record may exceed the store's record bound.
    fn every_record_fits(out: &[Effect]) {
        for record in records(out) {
            let len = crate::journal::encoded_len(record).unwrap();
            assert!(
                u64::try_from(len).unwrap() <= MAX_RECORD,
                "a {len}-byte record is over the record bound: {:?}",
                std::mem::discriminant(record)
            );
        }
    }

    #[test]
    fn the_record_bound_admits_an_exact_line_and_refuses_one_byte_more() {
        let bound = usize::try_from(MAX_RECORD).unwrap();

        // Measure the candidate line with a one-character payload, then pad
        // the payload by exactly the headroom left to the bound.
        let (_, _, probe) = run(padded(1));
        let (probe_record, _) = only_assistant(&probe);
        let probe_len = encode(probe_record).unwrap().len();
        assert!(probe_len < bound, "the probe line must leave headroom");
        let exact_chars = 1 + (bound - probe_len);
        drop(probe);

        // Exactly at the bound: the entry journals with its payload.
        let (session, _, out) = run(padded(exact_chars));
        let (record, kind) = only_assistant(&out);
        let line = encode(record).unwrap();
        assert_eq!(
            line.len(),
            bound,
            "the padded line must be exactly the record bound, line feed included"
        );
        let payload = replay_of(kind).expect("a line exactly at the bound journals its capture");
        assert_eq!(
            payload.as_str().len(),
            exact_chars + 2,
            "the whole payload rides the entry"
        );
        assert_eq!(content_of(kind), body());
        assert!(
            turn_ends(&out).is_empty() && stop_effects(&out).is_empty(),
            "a capture that fits fails nothing: {:?}",
            turn_ends(&out)
        );
        assert!(
            !matches!(session.phase(), Phase::Idle),
            "the turn continues past an accepted capture"
        );
        assert!(
            decode(&line).unwrap().record == *record,
            "the reader must accept the exact-bound line the writer emitted"
        );
        every_record_fits(&out);

        // The same entry with one more payload character is one byte over.
        let mut candidate = match record {
            Record::Assistant(entry) => entry.clone(),
            other => panic!("expected an assistant record: {other:?}"),
        };
        if let EntryKind::Assistant { replay, .. } = &mut candidate.kind {
            let mut json = String::with_capacity(exact_chars + 3);
            json.push('"');
            json.push_str(&"a".repeat(exact_chars + 1));
            json.push('"');
            *replay = Some(RawJson::parse(&json).unwrap());
        }
        let measured = encode(&Record::Assistant(candidate)).unwrap().len();
        assert_eq!(
            measured,
            bound + 1,
            "the over-bound candidate is one byte over"
        );
        drop((session, out, line));

        // One byte over: the content journals without the payload and the
        // turn ends Failed once, naming the token, the measured line and
        // the bound.
        let (session, _, out) = run(padded(exact_chars + 1));
        let (_, kind) = only_assistant(&out);
        assert!(
            replay_of(kind).is_none(),
            "an over-bound capture journals no payload"
        );
        assert_eq!(content_of(kind), body());
        let ends = turn_ends(&out);
        let [end] = ends.as_slice() else {
            panic!("the refusal ends the turn exactly once: {ends:?}");
        };
        let message = failed_token(end);
        assert!(
            message.contains(ReplayRefusal::LineBound.token()),
            "{message}"
        );
        assert!(message.contains(&measured.to_string()), "{message}");
        assert!(message.contains(&MAX_RECORD.to_string()), "{message}");
        assert_eq!(stop_effects(&out), [Stop::Failed]);
        assert_eq!(ended_updates(&out), [Stop::Failed]);
        assert!(matches!(session.phase(), Phase::Idle));
        every_record_fits(&out);
    }

    #[test]
    fn a_refusal_fails_the_turn_once_under_every_provider_stop() {
        for stop in STOPS {
            for reason in ADAPTER_REFUSALS {
                let case = format!("{stop:?} with {reason:?}");
                let (mut session, turn, out) = run(closing(vec![refused(reason)], stop));

                // The streamed content journals once, unchanged and without
                // a payload, under the provider's own stop.
                let (_, kind) = only_assistant(&out);
                assert!(replay_of(kind).is_none(), "{case}");
                assert_eq!(content_of(kind), body(), "{case}");
                let EntryKind::Assistant { stop: entry, .. } = kind else {
                    panic!("{case}: expected an assistant entry kind");
                };
                assert_eq!(*entry, assistant_stop(stop), "{case}");

                // One failed end naming the token; the provider's stop does
                // not end the turn a second time.
                let ends = turn_ends(&out);
                let [end] = ends.as_slice() else {
                    panic!("{case}: expected one turn end, got {ends:?}");
                };
                let message = failed_token(end);
                assert!(message.contains(reason.token()), "{case}: {message}");
                assert_eq!(stop_effects(&out), [Stop::Failed], "{case}");
                assert_eq!(ended_updates(&out), [Stop::Failed], "{case}");
                assert!(
                    !records(&out).any(|record| matches!(record, Record::ToolResult(_))),
                    "{case}: no call exists to settle"
                );
                assert!(matches!(session.phase(), Phase::Idle), "{case}");

                // A late repeat of the stream end finds no streaming turn.
                let again = send(
                    &mut session,
                    Event::StreamEnded {
                        turn,
                        model: route(),
                        family: Family::Chat,
                        result: Ok(closing(vec![], stop)),
                        partial: None,
                    },
                )
                .unwrap();
                assert!(again.is_empty(), "{case}: a second end_turn ran: {again:?}");
            }
        }
    }

    #[test]
    fn a_capture_on_a_tool_call_response_is_ignored_under_every_stop() {
        for stop in STOPS {
            let (baseline_session, _, baseline) = run(closing(vec![tool_call()], stop));
            for capture in [
                captured(r#"{"records":["r1"]}"#),
                refused(ReplayRefusal::NoRecords),
            ] {
                // The capture event sits on either side of the call.
                for (before, after) in [(true, false), (false, true)] {
                    let mut between = Vec::new();
                    if before {
                        between.push(capture.clone());
                    }
                    between.push(tool_call());
                    if after {
                        between.push(capture.clone());
                    }
                    let (session, _, out) = run(closing(between, stop));
                    assert_eq!(
                        out, baseline,
                        "{stop:?}: the capture changed the fold's effects"
                    );
                    assert_eq!(
                        session, baseline_session,
                        "{stop:?}: the capture changed the session"
                    );
                }
            }
            let (_, kind) = only_assistant(&baseline);
            assert!(replay_of(kind).is_none(), "{stop:?}");
            assert!(
                !turn_ends(&baseline).iter().any(|end| matches!(
                    end,
                    TurnEndStop::Failed { message }
                        if ADAPTER_REFUSALS.iter().any(|reason| message.contains(reason.token()))
                )),
                "{stop:?}: no refusal token may surface"
            );
        }
        let (session, _, _) = run(closing(
            vec![tool_call(), refused(ReplayRefusal::NoRecords)],
            Stop::EndTurn,
        ));
        assert!(
            matches!(
                session.phase(),
                Phase::Running {
                    stage: TurnStage::Resolving { .. },
                    ..
                }
            ),
            "the call is still pending after a refusal on its response: {:?}",
            session.phase()
        );
    }

    #[test]
    fn a_capture_alone_opens_no_entry_and_needs_no_entry_id() {
        // No stop and no content: the capture is not visible output, so the
        // preflight reserves no entry, the fold journals none, and the
        // response ends the turn as a stream without a stop.
        let mut session = session();
        let turn = begin(&mut session);
        session.next_entry = None;
        let out = stream_result(
            &mut session,
            turn,
            Inference {
                events: vec![captured(r#"{"records":["r1"]}"#)],
            },
        );
        assert!(
            !records(&out).any(|record| matches!(record, Record::Assistant(_))),
            "a capture alone opens no entry"
        );
        let ends = turn_ends(&out);
        let [TurnEndStop::Failed { .. }] = ends.as_slice() else {
            panic!("a stop-less response fails the turn once: {ends:?}");
        };
        assert!(matches!(session.phase(), Phase::Idle));
    }

    #[test]
    fn a_capture_with_a_stop_reserves_the_one_entry_it_rides() {
        let response = || Inference {
            events: vec![
                captured(r#"{"records":["r1"]}"#),
                StreamEvent::Usage(usage(10)),
                StreamEvent::Stop(Stop::EndTurn),
            ],
        };
        let mut session = session();
        let turn = begin(&mut session);
        session.next_entry = None;
        let before = session.clone();
        let error = send(
            &mut session,
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(response()),
                partial: None,
            },
        )
        .expect_err("the closing entry needs an id before the fold changes state");
        assert!(matches!(error, Rejection::Invalid { .. }));
        assert_eq!(session, before, "a rejected stream end changed the session");

        // The last id of the space is enough for the entry and its capture.
        session.next_entry = Some(entry(u64::MAX));
        let out = stream_result(&mut session, turn, response());
        let (_, kind) = only_assistant(&out);
        assert_eq!(
            replay_of(kind).map(RawJson::as_str),
            Some(r#"{"records":["r1"]}"#)
        );
        assert_eq!(turn_ends(&out), Vec::<&TurnEndStop>::new());
    }

    /// Debug half of GAP-3c: the protocol violations trip their assertions.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "duplicate message replay event")]
    fn a_duplicate_capture_trips_the_debug_assertion() {
        let _ = run(closing(
            vec![
                captured(r#"{"records":["first"]}"#),
                captured(r#"{"records":["second"]}"#),
            ],
            Stop::EndTurn,
        ));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "after its response's usage")]
    fn a_capture_after_the_usage_trips_the_debug_assertion() {
        let _ = run(late(captured(r#"{"records":["late"]}"#)));
    }

    /// Release half of GAP-3c: the first capture decides and is never
    /// overwritten, whichever kinds follow it.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_second_capture_never_overwrites_the_first() {
        let first = r#"{"records":["first"]}"#;
        let second = r#"{"records":["second"]}"#;

        let (_, _, out) = run(closing(
            vec![captured(first), captured(second)],
            Stop::EndTurn,
        ));
        let (_, kind) = only_assistant(&out);
        assert_eq!(replay_of(kind).map(RawJson::as_str), Some(first));
        assert_eq!(turn_ends(&out), Vec::<&TurnEndStop>::new());

        let (_, _, out) = run(closing(
            vec![captured(first), refused(ReplayRefusal::NoRecords)],
            Stop::EndTurn,
        ));
        let (_, kind) = only_assistant(&out);
        assert_eq!(replay_of(kind).map(RawJson::as_str), Some(first));
        assert_eq!(turn_ends(&out), Vec::<&TurnEndStop>::new());

        let (_, _, out) = run(closing(
            vec![refused(ReplayRefusal::NoRecords), captured(second)],
            Stop::EndTurn,
        ));
        let (_, kind) = only_assistant(&out);
        assert!(replay_of(kind).is_none());
        let ends = turn_ends(&out);
        let [end] = ends.as_slice() else {
            panic!("the first capture's refusal ends the turn once: {ends:?}");
        };
        assert!(failed_token(end).contains(ReplayRefusal::NoRecords.token()));
    }

    /// Release half of GAP-3c: a capture outside the window is dropped, and
    /// never displaces one accepted inside it.
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_capture_after_the_usage_is_dropped() {
        let (_, _, out) = run(late(captured(r#"{"records":["late"]}"#)));
        let (_, kind) = only_assistant(&out);
        assert!(replay_of(kind).is_none());
        assert_eq!(turn_ends(&out), Vec::<&TurnEndStop>::new());

        let (_, _, out) = run(late(refused(ReplayRefusal::NoRecords)));
        let (_, kind) = only_assistant(&out);
        assert!(replay_of(kind).is_none());
        assert!(
            turn_ends(&out).is_empty(),
            "a late refusal must not fail the turn"
        );

        let early = r#"{"records":["early"]}"#;
        let mut response = closing(vec![captured(early)], Stop::EndTurn);
        let stop = response.events.pop().unwrap();
        response.events.push(captured(r#"{"records":["late"]}"#));
        response.events.push(stop);
        let (_, _, out) = run(response);
        let (_, kind) = only_assistant(&out);
        assert_eq!(replay_of(kind).map(RawJson::as_str), Some(early));
        assert_eq!(turn_ends(&out), Vec::<&TurnEndStop>::new());
    }
}

fn provider_scoped_route(provider: &str) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "test-model".into(),
        provider: Some(provider.into()),
    }
}

fn measured_fixture() -> Vec<Record> {
    vec![
        user_question(1, None),
        route_assistant(
            2,
            Some(entry(1)),
            Some(AssistantRoute::Api(Family::Chat)),
            Family::Chat,
            "test-model",
            5_500,
        ),
    ]
}

fn select_model_recorded(session: &mut Session, journal: &mut Vec<Record>, route: ModelRoute) {
    let out = send(
        session,
        Event::Command {
            cmd: Command::SetModel {
                model: route,
                save: crate::Save::SessionOnly,
            },
            by: client(),
        },
    )
    .expect("selecting a model stays idle");
    append_emitted(&out, journal);
}

#[test]
fn context_measurement_accepts_a_gained_provider_not_a_changed_one() {
    let bare = ModelRoute::Api {
        family: Family::Chat,
        model: "test-model".into(),
        provider: None,
    };
    let renamed = ModelRoute::Api {
        family: Family::Chat,
        model: "other-model".into(),
        provider: Some("acme".into()),
    };
    let other_family = ModelRoute::Api {
        family: Family::Responses,
        model: "test-model".into(),
        provider: None,
    };
    // Usage journaled before providers travelled on routes names no provider,
    // so it has nothing to disagree with: every same family+model route
    // measures, whatever provider it names. Only a different model id or a
    // different family (or kind) breaks continuity here.
    let fixture = measured_fixture();
    for (route, expected) in [
        (bare.clone(), Some(5_500)),
        (provider_scoped_route("acme"), Some(5_500)),
        (provider_scoped_route("other"), Some(5_500)),
        (renamed.clone(), None),
        (other_family.clone(), None),
    ] {
        let mut live = Session::replay_declared(fixture.iter().cloned()).expect("fixture replays");
        let mut journal = decode_journal(&fixture);
        select_model_recorded(&mut live, &mut journal, route.clone());
        let journal = decode_journal(&journal);
        let replayed = Session::replay_declared(journal.iter().cloned()).expect("journal replays");
        assert_eq!(live.measured_context_tokens(), expected, "{route:?}");
        assert_eq!(replayed.measured_context_tokens(), expected, "{route:?}");
        if let Some(tokens) = expected {
            assert_eq!(live.tokens_since_last_compaction(), tokens, "{route:?}");
            assert_eq!(replayed.tokens_since_last_compaction(), tokens, "{route:?}");
        } else {
            assert_eq!(
                live.tokens_since_last_compaction(),
                replayed.tokens_since_last_compaction(),
                "{route:?} must estimate identically live and replayed",
            );
        }
    }
    // A turn answered by a provider-scoped route records that provider on its
    // usage, so selecting another provider for the same family+model rejects
    // the measurement while the bare and same-provider routes still accept.
    // Replay rebuilds usage identity from provenance, which carries no
    // provider, so only the live side can tell two providers apart; the
    // replayed side is asserted solely where both sides must agree.
    let mut live = session();
    let mut journal = Vec::new();
    let turn = open_turn(&mut live, &mut journal);
    let scoped = provider_scoped_route("acme");
    let streamed = send(
        &mut live,
        Event::StreamEnded {
            turn,
            model: scoped.clone(),
            family: Family::Chat,
            result: Ok(inference(Stop::EndTurn, &[], 5_500)),
            partial: None,
        },
    )
    .expect("the scoped response streams");
    append_emitted(&streamed, &mut journal);
    let ended = send(&mut live, Event::Boundary { turn }).expect("the turn ends");
    append_emitted(&ended, &mut journal);
    assert!(matches!(live.phase(), Phase::Idle));
    assert_eq!(live.measured_context_tokens(), Some(5_500));
    for (route, expected) in [
        (bare, Some(5_500)),
        (scoped, Some(5_500)),
        (provider_scoped_route("other"), None),
        (renamed, None),
        (other_family, None),
    ] {
        let mut branched = live.clone();
        let mut branch_journal = decode_journal(&journal);
        select_model_recorded(&mut branched, &mut branch_journal, route.clone());
        assert_eq!(branched.measured_context_tokens(), expected, "{route:?}");
        if let Some(tokens) = expected {
            assert_eq!(branched.tokens_since_last_compaction(), tokens, "{route:?}");
            let branch_journal = decode_journal(&branch_journal);
            let replayed =
                Session::replay_declared(branch_journal.iter().cloned()).expect("journal replays");
            assert_eq!(replayed.measured_context_tokens(), expected, "{route:?}");
            assert_eq!(replayed.tokens_since_last_compaction(), tokens, "{route:?}");
        }
    }
}

#[test]
fn leaf_switch_keeps_the_full_provider_route() {
    let scoped = provider_scoped_route("acme");
    let mut live = Session::replay_declared(measured_fixture()).expect("fixture replays");
    let mut journal = decode_journal(&measured_fixture());
    select_model_recorded(&mut live, &mut journal, scoped.clone());
    let model_entry = journal
        .iter()
        .find_map(|record| match record {
            Record::Model(entry) => Some(entry.id),
            _ => None,
        })
        .expect("SetModel journals a model record");
    assert_eq!(live.requested_model(), Some(&scoped));
    // `Fork` never runs inside the fold (the driver owns the copy), so the
    // in-fold leaf switch is `MoveLeaf`: the requested route is a setting,
    // not branch state, and must survive moving off the branch that set it.
    for leaf in [entry(1), model_entry] {
        let out = send(
            &mut live,
            Event::Command {
                cmd: Command::MoveLeaf(leaf),
                by: client(),
            },
        )
        .expect("leaf switch stays idle");
        append_emitted(&out, &mut journal);
        assert_eq!(
            live.requested_model(),
            Some(&scoped),
            "leaf {leaf} keeps the route",
        );
    }
    let journal = decode_journal(&journal);
    let rebuilt = Session::replay_declared(journal.iter().cloned()).expect("journal replays");
    assert_eq!(rebuilt.requested_model(), Some(&scoped));
}
