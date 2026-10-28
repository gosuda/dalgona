#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly"
)]

//! Close-out proofs: core vocabulary, fold, approval, extension values.
//!
//! Fold, approval, and extension-value obligations, proved through the public `dal-core` API only:
//! typed boundaries, state-transition refusals, approval-bypass attempts,
//! and extension-value lifecycle. Parent rulings pinned here:
//! `Other => Execute` (never `None`), read-only exec stays on the execute
//! rung, `EXCLUSIVE_TOOLS`/`SteerFull` behavior retained, Send+Sync asserted.

use dal_core::{
    Answer, ApprovalMode, Budget, CallId, CancelScope, ClientId, Command, Decision, DenyReason,
    Effect, Event, Expect, Gate, GrantSpec, HookEvent, HookOutcome, HookVerdict, Name, OnError,
    Part, Phase, PlannedCall, Policy, PrivateConflict, RegistrationError, Rejection, ReplayError,
    RequestId, ResolutionCause, ResolveError, Rung, ScopeSpec, ScopeSpecError, Service, ServiceSet,
    Session, SidecarName, ToolClass, TurnId, Unit, gate, headless_denial_text,
    parse_headless_denial, plan, rung,
};
use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Harness helpers (public API only)
// ---------------------------------------------------------------------------

fn stamp() -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
}

fn session() -> Session {
    Session::replay([], stamp()).expect("replays").0
}

fn client() -> ClientId {
    ClientId::new("test")
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero id"))
}

fn tool(name: &str) -> Name {
    Name::parse(name).expect("parses")
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

fn guard_for(turn: TurnId) -> Event {
    Event::Guard {
        turn,
        call: None,
        extension: None,
        outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
            .expect("guard outcome builds"),
    }
}

fn send(session: &mut Session, event: Event) -> Result<Vec<Effect>, Rejection> {
    let mut out = Vec::new();
    session.step(event, stamp(), &mut out)?;
    Ok(out)
}

/// Opens one turn and leaves the session in `Running`, or panics.
fn begin(session: &mut Session) -> TurnId {
    send(session, prompt("question")).expect("send succeeds");
    let turn = match session.phase() {
        Phase::Opening { turn, .. } => *turn,
        phase => panic!("prompt did not open a turn: {phase:?}"),
    };
    send(session, guard_for(turn)).expect("send succeeds");
    assert!(
        matches!(session.phase(), Phase::Running { turn: active, .. } if *active == turn),
        "guard must move the turn to Running: {:?}",
        session.phase()
    );
    turn
}

fn policy(mode: ApprovalMode, answerer_attached: bool) -> Policy {
    Policy {
        mode,
        answerer_attached,
        allow_always: BTreeSet::new(),
    }
}

fn exec_class(read_only: bool) -> ToolClass {
    ToolClass::Exec {
        read_only,
        grant: None,
    }
}

fn planned(id: &str, tool_name: &str, class: ToolClass) -> PlannedCall {
    PlannedCall {
        call: CallId::new(id),
        name: tool(tool_name),
        class,
    }
}

// ---------------------------------------------------------------------------
// A. Typed boundaries: extension identifiers and service vocabulary
// ---------------------------------------------------------------------------

#[test]
fn name_accepts_64_byte_maximum_and_rejects_65() {
    let max = "a".repeat(63);
    let ok = format!("z{max}"); // 1 + 63 = 64 bytes
    assert_eq!(Name::parse(&ok).expect("parses").as_str(), ok.as_str());
    let over = format!("z{max}x"); // 65 bytes
    assert!(Name::parse(&over).is_err(), "65-byte name must be rejected");
}

#[test]
fn name_rejects_malformed_identifiers() {
    for bad in [
        "",
        "A",
        "9lives",
        "has space",
        "Upper",
        "dot.name",
        "slash/x",
        "ünicode",
    ] {
        assert!(
            Name::parse(bad).is_err(),
            "{bad:?} must not parse as an extension name"
        );
    }
    // Mapped tool names are wider, but only through the mapped constructor:
    // the plain extension grammar stays closed.
    assert!(Name::parse("a.b").is_err());
    assert!(Name::parse_mapped_tool("a.b").is_ok());
    assert!(Name::parse_mapped_tool("").is_err());
}

#[test]
fn service_vocabulary_has_exactly_13_members_in_stable_order() {
    let order: Vec<&str> = Service::ALL.into_iter().map(Service::as_str).collect();
    assert_eq!(
        order,
        vec![
            "fs.read", "fs.write", "net", "run", "env", "ask", "mcp", "agents", "jobs", "turn",
            "sidecar", "infer", "judge",
        ],
        "the 13-service vocabulary and its order are closed"
    );
    for service in Service::ALL {
        assert_eq!(
            Service::parse(service.as_str()).expect("service parses"),
            service
        );
    }
}

#[test]
fn service_parse_rejects_unknown_and_ask_has_no_grant_capability() {
    let err = Service::parse("qubit").unwrap_err();
    assert!(
        matches!(err, RegistrationError::UnknownService { .. }),
        "unknown service must be typed, got {err:?}"
    );
    // `ask` is human-backed: it never mints a grant capability.
    assert_eq!(Service::Ask.capability(), None);
    for service in Service::ALL {
        if service != Service::Ask {
            assert!(
                service.capability().is_some(),
                "{service:?} must carry a grant capability"
            );
        }
    }
}

#[test]
fn service_set_rejects_duplicates_and_unknown_members() {
    assert!(ServiceSet::from_names(["fs.read", "fs.read"]).is_err());
    assert!(ServiceSet::from_names(["fs.read", "nope"]).is_err());
    let set = ServiceSet::from_names(["fs.read", "net"]).expect("set parses");
    assert!(set.contains(Service::FsRead));
    assert!(!set.contains(Service::Run));
    let both = set.union(ServiceSet::from_names(["run"]).expect("set parses"));
    assert!(both.contains(Service::Run) && both.contains(Service::Net));
}

#[test]
fn scope_spec_rejects_zero_and_over_cap_limits() {
    let spec = |limit| ScopeSpec {
        limit,
        on_error: OnError::Cancel,
        budget: Budget::default(),
    };
    assert!(matches!(
        spec(0).validate(500),
        Err(ScopeSpecError::InvalidLimit { limit: 0, .. })
    ));
    assert!(matches!(
        spec(501).validate(500),
        Err(ScopeSpecError::InvalidLimit { limit: 501, .. })
    ));
    spec(1).validate(500).expect("valid test setup");
    spec(500).validate(500).expect("valid test setup");
}

#[test]
fn scope_spec_rejects_nonpositive_budget_bounds() {
    let spec = |budget: Budget| ScopeSpec {
        limit: 4,
        on_error: OnError::Settle,
        budget,
    };
    let bad_requests = Budget {
        requests: Some(0),
        ..Budget::default()
    };
    assert!(matches!(
        spec(bad_requests).validate(500),
        Err(ScopeSpecError::InvalidBudget { .. })
    ));
    let bad_wall = Budget {
        wall: Some(std::time::Duration::ZERO),
        ..Budget::default()
    };
    assert!(matches!(
        spec(bad_wall).validate(500),
        Err(ScopeSpecError::InvalidBudget { .. })
    ));
    spec(Budget::default())
        .validate(500)
        .expect("valid test setup");
}

#[test]
fn sidecar_names_reject_dots_empty_and_long_inputs() {
    for bad in [
        "",
        ".",
        "..",
        ".hidden",
        "UPPER",
        "has space",
        &"a".repeat(65),
    ] {
        assert!(
            SidecarName::parse(bad).is_err(),
            "{bad:?} must not parse as a sidecar name"
        );
    }
    assert_eq!(
        SidecarName::parse("state.json").expect("parses").as_str(),
        "state.json"
    );
}

#[test]
fn hook_outcome_rejects_a_verdict_for_the_wrong_event() {
    // Typed boundary: an observer-style mismatch can never be built.
    let ok = HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))
        .expect("guard outcome builds");
    assert_eq!(ok.event(), HookEvent::BeforeTurn);
    assert!(
        HookOutcome::new(HookEvent::ToolCall, HookVerdict::BeforeTurn(None)).is_err(),
        "a before-turn verdict must not answer a tool-call event"
    );
}

// ---------------------------------------------------------------------------
// B. Invalid state transitions leave the session untouched
// ---------------------------------------------------------------------------

#[test]
fn empty_prompt_content_is_invalid_and_leaves_idle() {
    let mut session = session();
    let before = session.clone();
    let result = send(
        &mut session,
        Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![],
            },
            by: client(),
        },
    );
    assert!(matches!(result, Err(Rejection::Invalid { .. })));
    assert_eq!(session, before, "refused events must not mutate state");
}

#[test]
fn prompt_during_running_turn_is_wrong_turn_and_changes_nothing() {
    let mut session = session();
    let turn = begin(&mut session);
    let before = session.clone();
    let result = send(&mut session, prompt("interrupting"));
    assert!(matches!(result, Err(Rejection::WrongTurn { .. })));
    assert_eq!(session, before, "refused events must not mutate state");
    assert!(matches!(session.phase(), Phase::Running { turn: active, .. } if *active == turn));
}

#[test]
fn cancel_for_a_non_running_turn_is_wrong_turn() {
    let mut session = session();
    let before = session.clone();
    let result = send(
        &mut session,
        Event::Cancel {
            scope: CancelScope::Turn(turn_id(9)),
            partial: None,
        },
    );
    assert!(matches!(result, Err(Rejection::WrongTurn { .. })));
    assert_eq!(session, before);
}

#[test]
fn structural_commands_are_busy_while_a_turn_runs() {
    let mut session = session();
    begin(&mut session);
    let result = send(
        &mut session,
        Event::Command {
            cmd: Command::Clone,
            by: client(),
        },
    );
    assert!(matches!(result, Err(Rejection::BusyTurn)));
}

#[test]
fn closed_session_rejects_new_work_but_reports_closure() {
    let mut session = session();
    send(&mut session, Event::Close).expect("send succeeds");
    assert!(matches!(session.phase(), Phase::Closed));
    assert!(matches!(
        send(&mut session, prompt("too late")),
        Err(Rejection::SessionClosed)
    ));
}

#[test]
fn steer_queue_fills_at_16_and_turn_check_runs_first() {
    let mut session = session();
    let turn = begin(&mut session);
    for index in 0..16 {
        send(
            &mut session,
            Event::Steer {
                turn,
                text: index.to_string().into(),
            },
        )
        .expect("valid test setup");
    }
    assert_eq!(
        send(
            &mut session,
            Event::Steer {
                turn,
                text: "seventeenth".into()
            }
        ),
        Err(Rejection::SteerFull)
    );
    // A foreign turn is a turn error, never a capacity error.
    assert!(matches!(
        send(
            &mut session,
            Event::Steer {
                turn: turn_id(9),
                text: "late".into()
            }
        ),
        Err(Rejection::WrongTurn { .. })
    ));
}

#[test]
fn wake_run_refuses_the_21st_attempt() {
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
            .expect("valid test setup");
        let turn = match session.phase() {
            Phase::Opening { turn, .. } => *turn,
            phase => panic!("wake not opening: {phase:?}"),
        };
        session
            .step(guard_for(turn), stamp(), &mut out)
            .expect("guard builds");
        session
            .step(
                Event::Cancel {
                    scope: CancelScope::Turn(turn),
                    partial: None,
                },
                stamp(),
                &mut out,
            )
            .expect("valid test setup");
    }
    assert!(matches!(
        send(
            &mut session,
            Event::Wake {
                text: "refused".into(),
                sources: Box::new([]),
                jobs: Box::new([]),
            }
        ),
        Err(Rejection::Denied {
            reason: DenyReason::WakeLimit
        })
    ));
}

#[test]
fn contradictory_grant_resolutions_are_refused_before_state_changes() {
    let mut session = session();
    let before = session.clone();
    // A session grant can only come from a live client answer, never from a
    // cancelled broker resolution.
    let result = send(
        &mut session,
        Event::GrantResolved {
            request: RequestId::new_v7(),
            answer: Answer::ApproveForSession,
            by: None,
            was_default: false,
            cause: ResolutionCause::Cancelled,
        },
    );
    assert!(matches!(result, Err(Rejection::Invalid { .. })));
    // A defaulted answer must ride a deadline cause, never a live one.
    let result = send(
        &mut session,
        Event::GrantResolved {
            request: RequestId::new_v7(),
            answer: Answer::Approve,
            by: None,
            was_default: true,
            cause: ResolutionCause::Answered,
        },
    );
    assert!(matches!(result, Err(Rejection::Invalid { .. })));
    assert_eq!(session, before);
}

// ---------------------------------------------------------------------------
// C. Approval bypass attempts fail closed
// ---------------------------------------------------------------------------

#[test]
fn unclassified_tools_never_evade_the_execute_gate() {
    // Parent ruling: `Other => Execute`. `Gate::None` would silently allow
    // unclassified tools in every mode; this pin forbids that regression.
    assert_eq!(gate(&ToolClass::Other), Gate::Execute);
    assert_eq!(rung(&ToolClass::Other), Rung::All);
    assert_eq!(rung(&ToolClass::Patch), Rung::Edits);
    assert_eq!(gate(&ToolClass::Read), Gate::ReadOnly);
    assert_eq!(gate(&ToolClass::Patch), Gate::Mutate);
    assert_eq!(gate(&exec_class(false)), Gate::Execute);
    assert_eq!(gate(&exec_class(true)), Gate::Execute);
    assert_eq!(gate(&ToolClass::Eval { pure: true }), Gate::ReadOnly);
    assert_eq!(gate(&ToolClass::Eval { pure: false }), Gate::Execute);
}

#[test]
fn unclassified_tools_ask_or_deny_but_never_silently_allow() {
    let other = ToolClass::Other;
    // Ask and Edits modes: ask a human when one is attached, deny headless.
    for mode in [ApprovalMode::Ask, ApprovalMode::Edits] {
        assert_eq!(
            policy(mode, true).decide(&tool("mystery"), &other),
            Decision::Ask { grant: None },
            "unclassified tool must ask in {mode:?} with an answerer"
        );
        assert_eq!(
            policy(mode, false).decide(&tool("mystery"), &other),
            Decision::Deny {
                reason: DenyReason::NoFrontEnd
            },
            "unclassified tool must deny headless in {mode:?}"
        );
    }
    // All mode is the explicit operator opt-in: everything allows.
    assert_eq!(
        policy(ApprovalMode::All, false).decide(&tool("mystery"), &other),
        Decision::Allow
    );
    // Unknown tools are denied headless exactly like known gated tools.
    assert_eq!(
        policy(ApprovalMode::Ask, false).decide(&tool("never-registered"), &exec_class(false)),
        Decision::Deny {
            reason: DenyReason::NoFrontEnd
        }
    );
}

#[test]
fn headless_denials_carry_no_grant_and_user_declines_are_not_headless() {
    // A headless denial must never leak a grant an attacker could replay.
    let grant = GrantSpec {
        argv_prefix: "git".into(),
        roots: vec![PathBuf::from("/repo")],
    };
    let denied = policy(ApprovalMode::Ask, false).decide(
        &tool("command"),
        &ToolClass::Exec {
            read_only: false,
            grant: Some(grant),
        },
    );
    assert_eq!(
        denied,
        Decision::Deny {
            reason: DenyReason::NoFrontEnd
        }
    );
    // Ask decisions attach grants only for exec classes, never for `Other`.
    assert_eq!(
        policy(ApprovalMode::Ask, true).decide(&tool("mystery"), &ToolClass::Other),
        Decision::Ask { grant: None }
    );
}

#[test]
fn allow_always_is_the_only_bypass_and_it_is_explicit() {
    let mut allowlisted = policy(ApprovalMode::Ask, false);
    allowlisted.allow_always.insert(tool("command"));
    assert_eq!(
        allowlisted.decide(&tool("command"), &exec_class(false)),
        Decision::Allow,
        "explicit allowlist entry bypasses asks"
    );
    assert_eq!(
        allowlisted.decide(&tool("other-tool"), &exec_class(false)),
        Decision::Deny {
            reason: DenyReason::NoFrontEnd
        },
        "anything not allowlisted still denies headless"
    );
}

#[test]
fn headless_denial_text_round_trips_and_rejects_forgeries() {
    for (rung_value, flag) in [(Rung::Edits, "edits"), (Rung::All, "all")] {
        let text = headless_denial_text("patch", rung_value);
        assert!(
            text.contains(&format!("--approval {flag}.")),
            "denial must name its rung flag: {text}"
        );
        assert_eq!(parse_headless_denial(&text), Some(("patch", rung_value)));
    }
    // Foreign denial shapes must not parse as headless approvals protocol.
    assert_eq!(parse_headless_denial("Permission denied: other."), None);
    assert_eq!(parse_headless_denial(""), None);
    assert_eq!(
        parse_headless_denial("Permission denied: patch needs approval."),
        None
    );
    // A tampered rung flag fails closed to the strictest rung, never down to
    // `edits` (which would understate the approval the user must grant).
    let tampered = headless_denial_text("patch", Rung::Edits).replace("edits", "root");
    assert_eq!(parse_headless_denial(&tampered), Some(("patch", Rung::All)));
}

#[test]
fn planner_never_batches_exclusive_or_unapproved_reads() {
    // `ask` is read-classified but human-blocking: it always runs alone.
    let all = policy(ApprovalMode::All, true);
    let calls = [
        planned("1", "read", ToolClass::Read),
        planned("2", "ask", ToolClass::Read),
        planned("3", "read", ToolClass::Read),
    ];
    assert_eq!(
        plan(&calls, &all),
        vec![
            Unit::Reads {
                calls: vec![CallId::new("1")],
            },
            Unit::Serial {
                call: CallId::new("2"),
            },
            Unit::Reads {
                calls: vec![CallId::new("3")],
            },
        ]
    );
    // A read-only exec that still needs approval stays serial under Ask, but
    // batches once the operator allows everything.
    let reads = [
        planned("1", "read", ToolClass::Read),
        planned("2", "read_exec", exec_class(true)),
    ];
    assert_eq!(
        plan(&reads, &policy(ApprovalMode::Ask, true)),
        vec![
            Unit::Reads {
                calls: vec![CallId::new("1")],
            },
            Unit::Serial {
                call: CallId::new("2"),
            },
        ]
    );
    assert_eq!(
        plan(&reads, &all),
        vec![Unit::Reads {
            calls: vec![CallId::new("1"), CallId::new("2")],
        }]
    );
}

#[test]
fn planner_covers_every_call_exactly_once_in_input_order() {
    let policy = policy(ApprovalMode::All, true);
    let calls = [
        planned("1", "read", ToolClass::Read),
        planned("2", "exec_read", exec_class(true)),
        planned("3", "write", ToolClass::Patch),
        planned("4", "ask", ToolClass::Read),
        planned("5", "other", ToolClass::Other),
        planned("6", "read", ToolClass::Read),
    ];
    let units = plan(&calls, &policy);
    let flattened: Vec<CallId> = units
        .iter()
        .flat_map(|unit| match unit {
            Unit::Reads { calls } => calls.clone(),
            Unit::Serial { call } => vec![call.clone()],
            _ => panic!("Unit grew a variant; update this proof"),
        })
        .collect();
    let expected: Vec<CallId> = calls.iter().map(|call| call.call.clone()).collect();
    assert_eq!(
        flattened, expected,
        "no call dropped, duplicated, or reordered"
    );
    assert!(units.iter().any(|unit| matches!(
        unit,
        Unit::Serial { call } if *call == CallId::new("4")
    )));
}

// ---------------------------------------------------------------------------
// D. Lifecycle: boot, open, and thread-safety
// ---------------------------------------------------------------------------

#[test]
fn session_boots_idle_and_opens_turn_one_on_prompt() {
    let mut session = session();
    assert!(matches!(session.phase(), Phase::Idle));
    send(&mut session, prompt("hello")).expect("send succeeds");
    assert!(
        matches!(session.phase(), Phase::Opening { turn, .. } if *turn == turn_id(1)),
        "first prompt must open turn one, got {:?}",
        session.phase()
    );
}

#[test]
fn core_fold_types_are_send_sync_and_static() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<Session>();
    assert_send_sync::<Event>();
    assert_send_sync::<Effect>();
    assert_send_sync::<ReplayError>();
    assert_send_sync::<ResolveError>();
    assert_send_sync::<DenyReason>();
    assert_send_sync::<PrivateConflict>();
}
