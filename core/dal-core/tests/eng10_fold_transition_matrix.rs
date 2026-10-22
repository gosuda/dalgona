#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly"
)]

//! ENG-10 fold state-transition matrix (FLD-03A, FLD-03B, FLD-05A, FLD-06A).
//!
//! Drives one `Session` into every reachable lifecycle phase through the
//! public event API, then fires one instance of every `Event` variant (with a
//! matching and a stale turn id) at each of them. Per `(phase, event)` cell it
//! proves the reducer contract that holds regardless of the cell's verdict:
//!
//! 1. a refused event leaves the session equal to its pre-step clone and
//!    leaves the caller's effect buffer either untouched or empty;
//! 2. an accepted event is deterministic (the same event on an equal clone
//!    yields the same session and effects);
//! 3. every record an accepted event emits survives the journal codec byte
//!    round trip, and appending it to the fixture's journal keeps the journal
//!    replayable (the fold never emits what its own replay rejects);
//! 4. a closed session stays closed.
//!
//! Specific transition refusals (overflowing usage totals, duplicate call ids,
//! stale turns) are then pinned by name.

use dal_core::{
    Answer, ApprovalMode, Block, CallId, CancelScope, ClientId, Command, CompactLimits,
    CompactOutcome, CompactionSummary, Effect, Event, Expect, Family, HookEvent, HookOutcome,
    HookVerdict, InferFailure, Inference, InferredPurpose, JobId, JobKind, JobOutcome, Mode,
    ModelRoute, Name, Output, Owner, Part, PartialResponse, Phase, Question, RawJson, Record,
    Rejection, ReplayCapture, Reply, Request, RequestId, ResolutionCause, ResolvedCall, Save,
    Session, SettledOutcome, Stop, StreamChannel, StreamEvent, StreamVerdict, ThinkingLevel,
    ToolClass, TurnId, Usage, decode, encode,
};
use std::num::NonZeroU64;
use std::time::Duration;

fn stamp() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}

fn turn(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero id"))
}

fn route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "matrix-model".into(),
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

fn raw(text: &str) -> RawJson {
    RawJson::parse(text).expect("json parses")
}

fn client() -> ClientId {
    ClientId::new("matrix")
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
        outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
            .expect("guard outcome builds"),
    }
}

fn inference(stop: Stop, calls: &[&str], tokens: u64) -> Inference {
    let mut events = vec![StreamEvent::Usage(usage(tokens))];
    for call in calls {
        events.push(StreamEvent::ToolCall {
            call: CallId::new(*call),
            name: "read_file".into(),
            args: raw("{}"),
        });
    }
    events.push(StreamEvent::Stop(stop));
    Inference { events }
}

fn inference_with(stop: Stop, usage: Usage) -> Inference {
    Inference {
        events: vec![
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: "answer".into(),
            },
            StreamEvent::Usage(usage),
            StreamEvent::Stop(stop),
        ],
    }
}

#[derive(Clone, Copy)]
enum HostileUsage {
    NegativePremium,
    NanCost,
}

/// A measurement the totals can never absorb: the fold must refuse it whole.
fn hostile(kind: HostileUsage) -> Usage {
    let mut hostile = usage(3);
    match kind {
        HostileUsage::NegativePremium => hostile.premium_requests = Some(-1.0),
        HostileUsage::NanCost => hostile.cost_usd = Some(f64::NAN),
    }
    hostile
}

fn compaction_summary(usage: Option<Usage>) -> CompactionSummary {
    CompactionSummary {
        compactor: Name::parse("compactor").expect("parses"),
        tokens_before: 100,
        tokens_after: 10,
        summary: Some("summary".into()),
        first_kept: None,
        replay: None,
        usage,
        parts: Vec::new(),
        parts_tokens: 0,
        letters: Vec::new(),
    }
}

fn resolved_reads(turn: TurnId, calls: &[&str]) -> Event {
    Event::Resolved {
        turn,
        calls: calls
            .iter()
            .map(|call| ResolvedCall {
                call: CallId::new(*call),
                name: Name::parse("read_file").expect("parses"),
                promoted: false,
                result: Ok(ToolClass::Read),
            })
            .collect(),
        answerer_attached: false,
    }
}

fn limits_event() -> Event {
    Event::Limits {
        window: 100_000,
        max_steps: 0,
        compact: CompactLimits {
            threshold: 0.85,
            min_tokens: 0,
            keep_tokens: 20_000,
            enabled: true,
            compactor_available: true,
        },
    }
}

/// A session, the journal its accepted steps emitted, and the turn it is on.
#[derive(Clone)]
struct Fixture {
    label: &'static str,
    session: Session,
    journal: Vec<Record>,
    turn: TurnId,
}

fn collect(effects: &[Effect], journal: &mut Vec<Record>) {
    for effect in effects {
        if let Effect::Emit(emit) = effect {
            journal.extend(emit.records.iter().cloned());
        }
    }
}

impl Fixture {
    fn fresh(label: &'static str) -> Self {
        let (session, effects) = Session::replay([], stamp()).expect("replays");
        let mut journal = Vec::new();
        collect(&effects, &mut journal);
        Self {
            label,
            session,
            journal,
            turn: turn(1),
        }
    }

    fn drive(&mut self, event: Event) -> Vec<Effect> {
        let mut out = Vec::new();
        self.session
            .step(event, stamp(), &mut out)
            .unwrap_or_else(|error| panic!("{}: fixture step refused: {error:?}", self.label));
        collect(&out, &mut self.journal);
        out
    }

    fn opening(mut self) -> Self {
        self.drive(prompt("matrix"));
        self.turn = match self.session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("{}: prompt did not open a turn: {phase:?}", self.label),
        };
        self
    }

    fn boundary(mut self) -> Self {
        let turn = self.turn;
        self.drive(guard(turn));
        self
    }

    fn streaming(mut self) -> Self {
        let turn = self.turn;
        self.drive(Event::RequestStarted {
            turn,
            model: route(),
            family: Family::Chat,
        });
        self
    }

    fn resolving(mut self) -> Self {
        let turn = self.turn;
        self.drive(Event::StreamEnded {
            turn,
            model: route(),
            family: Family::Chat,
            result: Ok(inference(Stop::EndTurn, &["c1", "c2"], 10)),
            partial: None,
        });
        self
    }

    fn dispatching(mut self) -> Self {
        let turn = self.turn;
        self.drive(resolved_reads(turn, &["c1", "c2"]));
        self
    }

    fn started(mut self) -> Self {
        let turn = self.turn;
        self.drive(Event::CallStarted {
            turn,
            call: CallId::new("c1"),
        });
        self
    }
}

fn fixtures() -> Vec<Fixture> {
    let idle = Fixture::fresh("idle");
    let mut ended = Fixture::fresh("idle_after_cancelled_turn")
        .opening()
        .boundary();
    let ended_turn = ended.turn;
    ended.drive(Event::Cancel {
        scope: CancelScope::Turn(ended_turn),
        partial: None,
    });
    let mut compacting = Fixture::fresh("compacting_manual");
    compacting.drive(limits_event());
    compacting.drive(Event::Command {
        cmd: Command::Compact { focus: None },
        by: client(),
    });
    let mut closed = Fixture::fresh("closed");
    closed.drive(Event::Close);

    vec![
        idle,
        ended,
        Fixture::fresh("opening").opening(),
        Fixture::fresh("running_boundary").opening().boundary(),
        Fixture::fresh("running_streaming")
            .opening()
            .boundary()
            .streaming(),
        Fixture::fresh("running_resolving")
            .opening()
            .boundary()
            .streaming()
            .resolving(),
        Fixture::fresh("running_dispatching")
            .opening()
            .boundary()
            .streaming()
            .resolving()
            .dispatching(),
        Fixture::fresh("running_dispatching_started")
            .opening()
            .boundary()
            .streaming()
            .resolving()
            .dispatching()
            .started(),
        compacting,
        closed,
    ]
}

/// Command half of the matrix: every `Command` variant as an `Event::Command`.
fn command_events(
    turn: TurnId,
    job: JobId,
    entry: dal_core::EntryId,
) -> Vec<(&'static str, Event)> {
    let commands = vec![
        (
            "cmd_prompt_idle",
            Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: "x".into() }],
            },
        ),
        (
            "cmd_prompt_after",
            Command::Prompt {
                expect: Expect::After(turn),
                content: vec![Part::Text { text: "x".into() }],
            },
        ),
        (
            "cmd_prompt_empty",
            Command::Prompt {
                expect: Expect::Idle,
                content: Vec::new(),
            },
        ),
        (
            "cmd_steer",
            Command::Steer {
                turn,
                content: vec![Part::Text { text: "x".into() }],
            },
        ),
        (
            "cmd_follow_up",
            Command::FollowUp {
                turn,
                content: vec![Part::Text { text: "x".into() }],
            },
        ),
        ("cmd_cancel_queued", Command::CancelQueued { turn }),
        (
            "cmd_cancel_turn",
            Command::Cancel {
                scope: CancelScope::Turn(turn),
            },
        ),
        (
            "cmd_cancel_job",
            Command::Cancel {
                scope: CancelScope::Job(job),
            },
        ),
        (
            "cmd_set_model",
            Command::SetModel {
                model: route(),
                save: Save::SessionOnly,
            },
        ),
        (
            "cmd_set_thinking",
            Command::SetThinking {
                level: ThinkingLevel::High,
                save: Save::SessionOnly,
            },
        ),
        (
            "cmd_set_approval",
            Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionOnly,
            },
        ),
        (
            "cmd_set_mode",
            Command::SetMode {
                mode: Mode::EvalOnly,
                save: Save::SessionOnly,
            },
        ),
        ("cmd_compact", Command::Compact { focus: None }),
        ("cmd_move_leaf", Command::MoveLeaf(entry)),
        ("cmd_fork", Command::Fork(entry)),
        ("cmd_clone", Command::Clone),
        ("cmd_rename", Command::Rename("renamed".into())),
        (
            "cmd_run",
            Command::Run {
                name: "noop".into(),
                args: "".into(),
                expected: None,
            },
        ),
        (
            "cmd_set_scoped_models",
            Command::SetScopedModels(vec!["a".into()]),
        ),
        ("cmd_reload_plugins", Command::ReloadPlugins),
    ];
    commands
        .into_iter()
        .map(|(label, cmd)| (label, Event::Command { cmd, by: client() }))
        .collect()
}

/// Guard, verdict, retry, inference, request-open, and grant events.
fn guard_and_grant_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    let request = Request {
        id: RequestId::new_v7(),
        turn: Some(turn),
        owner: Owner::Core,
        question: Question::Confirm {
            text: "continue?".into(),
        },
        timeout: Duration::from_secs(30),
        default: Answer::Cancel,
    };
    vec![
        ("guard", guard(turn)),
        (
            "stream_verdict_continue",
            Event::StreamVerdict {
                turn,
                verdict: StreamVerdict::Continue,
            },
        ),
        (
            "stream_verdict_interrupt",
            Event::StreamVerdict {
                turn,
                verdict: StreamVerdict::Interrupt {
                    rule: "r".into(),
                    inject: "i".into(),
                },
            },
        ),
        (
            "stream_retry",
            Event::StreamRetry {
                turn,
                reminder: None,
            },
        ),
        (
            "stream_reminder",
            Event::StreamReminder {
                turn,
                rule: "r".into(),
                text: "t".into(),
            },
        ),
        (
            "inferred",
            Event::Inferred {
                at: stamp(),
                who: Owner::Core,
                purpose: InferredPurpose::Synthetic {
                    id: "ns/name".into(),
                },
                usage: usage(1),
            },
        ),
        ("request_opened", Event::RequestOpened { request }),
        (
            "grant_resolved_unknown",
            Event::GrantResolved {
                request: RequestId::new_v7(),
                answer: Answer::Approve,
                by: Some(client()),
                was_default: false,
                cause: ResolutionCause::Answered,
            },
        ),
        (
            "grant_resolved_contradiction",
            Event::GrantResolved {
                request: RequestId::new_v7(),
                answer: Answer::ApproveForSession,
                by: None,
                was_default: true,
                cause: ResolutionCause::Cancelled,
            },
        ),
    ]
}

/// Job, wake, steer, cancel, stream, and request-started events.
fn job_lifecycle_events(turn: TurnId, job: JobId) -> Vec<(&'static str, Event)> {
    vec![
        (
            "job_started",
            Event::JobStarted {
                job,
                kind: JobKind::Exec,
            },
        ),
        (
            "job_settled",
            Event::JobSettled {
                job,
                outcome: JobOutcome::Exited { code: 0 },
            },
        ),
        (
            "wake",
            Event::Wake {
                text: "wake".into(),
                sources: Box::new([]),
                jobs: Box::new([]),
            },
        ),
        (
            "steer",
            Event::Steer {
                turn,
                text: "steer".into(),
            },
        ),
        (
            "cancel_turn",
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: None,
            },
        ),
        (
            "cancel_turn_partial",
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: Some(PartialResponse {
                    content: vec![Block::Text {
                        text: "partial".into(),
                        replay: None,
                    }],
                    usage: usage(3),
                }),
            },
        ),
        (
            "cancel_job",
            Event::Cancel {
                scope: CancelScope::Job(job),
                partial: None,
            },
        ),
        (
            "stream_delta",
            Event::Stream {
                turn,
                event: StreamEvent::Delta {
                    channel: StreamChannel::Text,
                    text: "hi".into(),
                },
            },
        ),
        (
            "stream_usage",
            Event::Stream {
                turn,
                event: StreamEvent::Usage(usage(2)),
            },
        ),
        (
            "request_started",
            Event::RequestStarted {
                turn,
                model: route(),
                family: Family::Chat,
            },
        ),
    ]
}

/// First half of the `StreamEnded` outcomes: clean results and hard failures.
fn stream_ended_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    vec![
        (
            "stream_ended_end_turn",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference(Stop::EndTurn, &[], 4)),
                partial: None,
            },
        ),
        (
            "stream_ended_calls",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference(Stop::EndTurn, &["x1", "x2"], 4)),
                partial: None,
            },
        ),
        (
            "stream_ended_duplicate_calls",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference(Stop::EndTurn, &["dup", "dup"], 4)),
                partial: None,
            },
        ),
        (
            "stream_ended_cancelled",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Cancelled),
                partial: None,
            },
        ),
        (
            "stream_ended_fatal",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Fatal {
                    message: "boom".into(),
                    fix: None,
                }),
                partial: None,
            },
        ),
        (
            "stream_ended_retryable",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Retryable {
                    hint: None,
                    message: "later".into(),
                }),
                partial: None,
            },
        ),
        (
            "stream_ended_overflow",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Overflow {
                    code: "context_length_exceeded".into(),
                    message: "too long".into(),
                }),
                partial: None,
            },
        ),
    ]
}

/// Second half of the `StreamEnded` outcomes: hostile usage totals.
fn stream_ended_hostile_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    vec![
        (
            "stream_ended_negative_premium",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference_with(
                    Stop::EndTurn,
                    hostile(HostileUsage::NegativePremium),
                )),
                partial: None,
            },
        ),
        (
            "stream_ended_nan_cost",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference_with(
                    Stop::EndTurn,
                    hostile(HostileUsage::NanCost),
                )),
                partial: None,
            },
        ),
        (
            "stream_ended_no_stop_nan_cost",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(Inference {
                    events: vec![
                        StreamEvent::Delta {
                            channel: StreamChannel::Text,
                            text: "cut off".into(),
                        },
                        StreamEvent::Usage(hostile(HostileUsage::NanCost)),
                    ],
                }),
                partial: None,
            },
        ),
        (
            "stream_ended_fatal_partial_nan_cost",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Err(InferFailure::Fatal {
                    message: "boom".into(),
                    fix: None,
                }),
                partial: Some(PartialResponse {
                    content: vec![Block::Text {
                        text: "partial".into(),
                        replay: None,
                    }],
                    usage: hostile(HostileUsage::NanCost),
                }),
            },
        ),
    ]
}

/// Tail of the stream block: partial cancels and compaction-shaped endings.
fn stream_ended_tail_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    vec![
        (
            "cancel_partial_nan_cost",
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: Some(PartialResponse {
                    content: vec![Block::Text {
                        text: "partial".into(),
                        replay: None,
                    }],
                    usage: hostile(HostileUsage::NanCost),
                }),
            },
        ),
        (
            "stream_ended_native_compaction_event",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(Inference {
                    events: vec![
                        StreamEvent::Compaction {
                            outcome: CompactOutcome::Unsupported,
                        },
                        StreamEvent::Stop(Stop::EndTurn),
                    ],
                }),
                partial: None,
            },
        ),
        (
            "stream_ended_null_replay_capture",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(Inference {
                    events: vec![
                        StreamEvent::MessageReplay {
                            capture: ReplayCapture::Captured {
                                payload: RawJson::null(),
                            },
                        },
                        StreamEvent::Stop(Stop::EndTurn),
                    ],
                }),
                partial: None,
            },
        ),
    ]
}

/// The accepted `CompactionSettled` outcomes, manual and turn-scoped.
fn compaction_settled_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    vec![
        (
            "compaction_settled_applied",
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Ok(compaction_summary(None)),
            },
        ),
        (
            "compaction_settled_applied_manual",
            Event::CompactionSettled {
                turn: None,
                outcome: Ok(compaction_summary(None)),
            },
        ),
        (
            "compaction_settled_nan_cost",
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Ok(compaction_summary(Some(hostile(HostileUsage::NanCost)))),
            },
        ),
        (
            "compaction_settled_nan_cost_manual",
            Event::CompactionSettled {
                turn: None,
                outcome: Ok(compaction_summary(Some(hostile(HostileUsage::NanCost)))),
            },
        ),
    ]
}

/// Dispatch, boundary, limits, failed-compaction, and close events.
fn dispatch_events(turn: TurnId) -> Vec<(&'static str, Event)> {
    vec![
        ("resolved_known", resolved_reads(turn, &["c1", "c2"])),
        ("resolved_unknown", resolved_reads(turn, &["nope"])),
        ("resolved_duplicate", resolved_reads(turn, &["c1", "c1"])),
        ("resolved_empty", resolved_reads(turn, &[])),
        (
            "call_started_known",
            Event::CallStarted {
                turn,
                call: CallId::new("c1"),
            },
        ),
        (
            "call_started_unknown",
            Event::CallStarted {
                turn,
                call: CallId::new("nope"),
            },
        ),
        (
            "settled_known",
            Event::Settled {
                turn,
                call: CallId::new("c1"),
                outcome: SettledOutcome::Ok {
                    text: "ok".into(),
                    data: None,
                },
                elapsed_ms: Some(1),
            },
        ),
        (
            "settled_unknown",
            Event::Settled {
                turn,
                call: CallId::new("nope"),
                outcome: SettledOutcome::Err { text: "no".into() },
                elapsed_ms: None,
            },
        ),
        ("boundary", Event::Boundary { turn }),
        ("limits", limits_event()),
        (
            "compaction_settled_failed_turn",
            Event::CompactionSettled {
                turn: Some(turn),
                outcome: Err("compactor failed".into()),
            },
        ),
        (
            "compaction_settled_failed_manual",
            Event::CompactionSettled {
                turn: None,
                outcome: Err("compactor failed".into()),
            },
        ),
        ("close", Event::Close),
    ]
}

/// One instance of every `Event` variant; `turn` is the target turn id.
fn catalogue(turn: TurnId) -> Vec<(&'static str, Event)> {
    let job = JobId::new_v7();
    let entry = dal_core::EntryId::new(NonZeroU64::MIN);
    let mut events = command_events(turn, job, entry);
    events.extend(guard_and_grant_events(turn));
    events.extend(job_lifecycle_events(turn, job));
    events.extend(stream_ended_events(turn));
    events.extend(stream_ended_hostile_events(turn));
    events.extend(stream_ended_tail_events(turn));
    events.extend(compaction_settled_events(turn));
    events.extend(dispatch_events(turn));
    events
}

fn sentinel() -> Vec<Effect> {
    vec![Effect::Reply(Ok(Reply::Done(Output::Nothing)))]
}

#[derive(Default)]
struct Tally {
    cells: usize,
    accepted: usize,
    refused: usize,
}

fn check_cell(fixture: &Fixture, label: &str, event: &Event, tally: &mut Tally) {
    let cell = format!("{} x {label}", fixture.label);
    let before = fixture.session.clone();
    let mut session = fixture.session.clone();
    let mut out = sentinel();
    let result = session.step(event.clone(), stamp(), &mut out);
    tally.cells += 1;

    if matches!(before.phase(), Phase::Closed) {
        assert!(
            matches!(session.phase(), Phase::Closed),
            "{cell}: a closed session must stay closed, got {:?}",
            session.phase()
        );
    }

    if let Err(rejection) = result {
        tally.refused += 1;
        assert_eq!(
            session, before,
            "{cell}: refused with {rejection:?} but the session changed"
        );
        assert!(
            out == sentinel() || out.is_empty(),
            "{cell}: refused with {rejection:?} but left effects in the caller's buffer: {out:?}"
        );
    } else {
        tally.accepted += 1;
        // Purity: an equal clone replays the same transition.
        let mut twin = fixture.session.clone();
        let mut twin_out = sentinel();
        twin.step(event.clone(), stamp(), &mut twin_out)
            .unwrap_or_else(|error| panic!("{cell}: nondeterministic refusal {error:?}"));
        assert_eq!(session, twin, "{cell}: transition is not deterministic");
        assert_eq!(out, twin_out, "{cell}: effects are not deterministic");

        // Every emitted record is encodable, decodable, and replayable.
        let mut emitted = Vec::new();
        collect(&out, &mut emitted);
        for record in &emitted {
            let bytes = encode(record).unwrap_or_else(|error| {
                panic!("{cell}: emitted record not encodable: {error:?}: {record:?}")
            });
            let decoded = decode(&bytes).unwrap_or_else(|error| {
                panic!("{cell}: emitted record not decodable: {error:?}: {record:?}")
            });
            assert_eq!(
                &decoded.record, record,
                "{cell}: record changed across the journal codec"
            );
        }
        let mut journal = fixture.journal.clone();
        journal.extend(emitted);
        let replayed = Session::replay_declared(journal).unwrap_or_else(|error| {
            panic!("{cell}: the fold emitted records its own replay rejects: {error:?}")
        });
        assert_eq!(
            replayed.approval_mode(),
            session.approval_mode(),
            "{cell}: approval mode is not reconstructible from the journal"
        );
        assert_eq!(
            replayed.allow_always(),
            session.allow_always(),
            "{cell}: the allow-always set is not reconstructible from the journal"
        );
    }
}

#[test]
fn every_event_in_every_phase_is_atomic_deterministic_and_replayable() {
    let mut tally = Tally::default();
    for fixture in fixtures() {
        for target in [fixture.turn, turn(99)] {
            for (label, event) in catalogue(target) {
                let label = if target == fixture.turn {
                    label.to_owned()
                } else {
                    format!("{label}@stale")
                };
                check_cell(&fixture, &label, &event, &mut tally);
            }
        }
    }
    assert!(tally.cells > 1000, "matrix shrank to {} cells", tally.cells);
    assert!(
        tally.accepted > 100,
        "only {} cells accepted",
        tally.accepted
    );
    assert!(tally.refused > 300, "only {} cells refused", tally.refused);
}

#[test]
fn fixtures_reach_every_declared_lifecycle_phase() {
    let mut seen = std::collections::BTreeSet::new();
    for fixture in fixtures() {
        let name = match fixture.session.phase() {
            Phase::Idle => "idle",
            Phase::Opening { .. } => "opening",
            Phase::Running { .. } => "running",
            Phase::Settling { .. } => "settling",
            Phase::Compacting { .. } => "compacting",
            Phase::Closed => "closed",
            other => panic!("new lifecycle phase needs a matrix fixture: {other:?}"),
        };
        seen.insert(name);
    }
    for required in ["idle", "opening", "running", "compacting", "closed"] {
        assert!(
            seen.contains(required),
            "no fixture reached {required}: {seen:?}"
        );
    }
}

/// A turn whose first response already folded `u64::MAX - 1` input tokens into
/// the turn totals and whose only call has settled, so the next response is
/// the one that would overflow them.
fn session_after_heavy_response() -> Fixture {
    let mut fixture = Fixture::fresh("heavy").opening().boundary().streaming();
    let turn = fixture.turn;
    fixture.drive(Event::StreamEnded {
        turn,
        model: route(),
        family: Family::Chat,
        result: Ok(inference(Stop::EndTurn, &["h1"], u64::MAX - 1)),
        partial: None,
    });
    fixture.drive(resolved_reads(turn, &["h1"]));
    fixture.drive(Event::CallStarted {
        turn,
        call: CallId::new("h1"),
    });
    fixture.drive(Event::Settled {
        turn,
        call: CallId::new("h1"),
        outcome: SettledOutcome::Ok {
            text: "ok".into(),
            data: None,
        },
        elapsed_ms: None,
    });
    fixture
}

#[test]
fn overflowing_turn_usage_is_refused_without_partial_mutation() {
    let fixture = session_after_heavy_response();
    let turn = fixture.turn;
    // The totals already hold almost `u64::MAX` input tokens; any further
    // usage-bearing input must be refused as a whole, not folded in part.
    let overflow = [
        (
            "stream_ended",
            Event::StreamEnded {
                turn,
                model: route(),
                family: Family::Chat,
                result: Ok(inference(Stop::EndTurn, &[], 10)),
                partial: None,
            },
        ),
        (
            "cancel_partial",
            Event::Cancel {
                scope: CancelScope::Turn(turn),
                partial: Some(PartialResponse {
                    content: vec![Block::Text {
                        text: "partial".into(),
                        replay: None,
                    }],
                    usage: usage(10),
                }),
            },
        ),
        (
            "inferred",
            Event::Inferred {
                at: stamp(),
                who: Owner::Core,
                purpose: InferredPurpose::Synthetic {
                    id: "ns/name".into(),
                },
                usage: usage(10),
            },
        ),
    ];
    for (label, event) in overflow {
        let before = fixture.session.clone();
        let mut session = fixture.session.clone();
        let mut out = sentinel();
        let result = session.step(event, stamp(), &mut out);
        match result {
            Err(Rejection::Invalid { .. }) => {
                assert_eq!(
                    session, before,
                    "{label}: refused overflow mutated the session"
                );
            }
            Err(other) => panic!("{label}: expected Invalid, got {other:?}"),
            Ok(()) => panic!("{label}: overflowing usage total was accepted"),
        }
    }
}

#[test]
fn duplicate_call_identities_never_dispatch_twice() {
    let mut fixture = Fixture::fresh("dup").opening().boundary().streaming();
    let turn = fixture.turn;
    fixture.drive(Event::StreamEnded {
        turn,
        model: route(),
        family: Family::Chat,
        result: Ok(inference(Stop::EndTurn, &["dup", "dup"], 4)),
        partial: None,
    });
    let mut session = fixture.session.clone();
    let mut out = Vec::new();
    let accepted = session
        .step(resolved_reads(turn, &["dup", "dup"]), stamp(), &mut out)
        .is_ok();
    let dispatched: Vec<&dal_core::Unit> = if accepted {
        out.iter()
            .filter_map(|effect| match effect {
                Effect::Dispatch { units, .. } => Some(units.as_slice()),
                _ => None,
            })
            .flatten()
            .collect()
    } else {
        Vec::new()
    };
    let planned: usize = dispatched
        .iter()
        .map(|unit| match unit {
            dal_core::Unit::Reads { calls } => {
                calls.iter().filter(|call| call.as_str() == "dup").count()
            }
            dal_core::Unit::Serial { call } => usize::from(call.as_str() == "dup"),
            _ => panic!("Unit grew a variant; update this proof"),
        })
        .sum();
    assert!(
        planned <= 1,
        "call id `dup` was planned for dispatch {planned} times"
    );
}

#[test]
fn stale_turn_events_never_touch_a_running_turn() {
    let running = Fixture::fresh("running").opening().boundary().streaming();
    let stale = turn(running.turn.get() + 7);
    for (label, event) in catalogue(stale) {
        let mut session = running.session.clone();
        let mut out = sentinel();
        if session.step(event, stamp(), &mut out).is_ok() {
            // Events that carry no turn id (commands that target the whole
            // session, limits, wakes, job bookkeeping) may legitimately pass;
            // the running turn itself must still be the one that was open.
            assert!(
                matches!(session.phase(), Phase::Running { turn, .. } if *turn == running.turn)
                    || matches!(session.phase(), Phase::Closed),
                "{label}: a stale-turn event moved the running turn to {:?}",
                session.phase()
            );
        }
    }
}

#[test]
fn approval_mode_changes_are_journaled_and_cannot_hide_in_a_busy_turn() {
    // Raising the approval mode is an approval-relevant transition: it must be
    // reconstructible from the journal when accepted, and never silently
    // applied by a refused command.
    let mut idle = Fixture::fresh("approval");
    assert_eq!(idle.session.approval_mode(), ApprovalMode::Ask);
    idle.drive(Event::Command {
        cmd: Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        },
        by: client(),
    });
    assert_eq!(idle.session.approval_mode(), ApprovalMode::All);
    let replayed = Session::replay(idle.journal.clone(), stamp())
        .expect("replays")
        .0;
    assert_eq!(replayed.approval_mode(), ApprovalMode::All);

    let busy = Fixture::fresh("busy").opening().boundary();
    let mut session = busy.session.clone();
    let before = session.clone();
    let mut out = Vec::new();
    let refused = session.step(
        Event::Command {
            cmd: Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionOnly,
            },
            by: client(),
        },
        stamp(),
        &mut out,
    );
    // Whichever way the reducer rules (accept as a live setting or refuse as
    // busy), a refusal must not have changed the mode and an acceptance must be
    // journaled.
    if refused.is_err() {
        assert_eq!(session, before);
    } else {
        let mut emitted = Vec::new();
        collect(&out, &mut emitted);
        assert!(
            emitted
                .iter()
                .any(|record| matches!(record, Record::Approval(_))),
            "an accepted approval change must be journaled: {emitted:?}"
        );
    }
}
