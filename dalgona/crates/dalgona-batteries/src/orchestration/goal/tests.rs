// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! State-machine tests for the sidecar codec, goal tools and commands, the
//! continuation verdict, and prompt construction.

use std::error::Error;

use dal_core::Timestamp;

use super::super::monitor::{InflightCounts, inflight_counts};
use super::super::{ControllerMode, GoalStatus, StopKind};
use super::ops::{
    GoalCommand, GoalScope, TodoSummary, UpdateTarget, apply_goal_command, clear_recovery_doc,
    continuation_line, continuation_unknown, continuation_unsaved, create_goal, format_duration,
    get_goal, parse_goal_command, salvage_next_goal, update_goal,
};
use super::policy::{
    DenyReason, GoalPath, PromptKind, Verdict, VerdictInput, on_user_prompt, progress_signature,
    record_goal_turn, verdict,
};
use super::prompt::{build_prompt, escape_objective};
use super::sidecar::{BlockedReason, Goal, GoalSidecar, decode_sidecar, encode_sidecar};

fn ts(text: &str) -> Result<Timestamp, Box<dyn Error>> {
    Ok(text.parse::<Timestamp>()?)
}

fn scope(session: &str) -> GoalScope<'_> {
    GoalScope {
        session,
        saved: true,
        depth: 0,
    }
}

fn empty_sidecar(session: &str) -> GoalSidecar {
    GoalSidecar {
        v: 1,
        session: session.into(),
        controller: ControllerMode::Run,
        next_goal: 1,
        goal: None,
    }
}

fn active_sidecar() -> Result<(GoalSidecar, GoalScope<'static>), Box<dyn Error>> {
    let session: &'static str = "s1";
    let mut sidecar = empty_sidecar(session);
    let ctx = GoalScope {
        session,
        saved: true,
        depth: 0,
    };
    create_goal(
        &mut sidecar,
        &ctx,
        "write the parser",
        ts("2026-09-25T10:15:30.123Z")?,
    )?;
    Ok((sidecar, ctx))
}

/// One verdict input over an active goal with idle defaults.
fn base_input<'a>(goal: &'a Goal, inflight: &'a InflightCounts) -> VerdictInput<'a> {
    VerdictInput {
        goal,
        path: GoalPath::AfterTurn,
        idle: true,
        pending_user_messages: false,
        continuation_pending: false,
        last_turn_context_overflow: false,
        last_stop: StopKind::Completed,
        signature: "g1:0/0:abcd1234",
        open_todos: 0,
        total_todos: 0,
        inflight,
    }
}

#[test]
fn sidecar_round_trip_matches_exact_shape() -> Result<(), Box<dyn Error>> {
    let created = ts("2026-09-25T10:15:30.123Z")?;
    let updated = ts("2026-09-25T10:20:00.000Z")?;
    let sidecar = GoalSidecar {
        v: 1,
        session: "s1".into(),
        controller: ControllerMode::Run,
        next_goal: 4,
        goal: Some(Goal {
            id: "g3".into(),
            objective: "...".into(),
            status: GoalStatus::Active,
            created_at: created,
            updated_at: updated,
            tokens_used: 12345,
            time_used_s: 300,
            consecutive: 0,
            unattended: 0,
            length_recoveries: 0,
            toolless_streak: 0,
            goal_turns: 0,
            last_signature: None,
            recent_hashes: Vec::new(),
            blocked: None,
            completed_at: None,
        }),
    };
    let bytes = encode_sidecar(&sidecar)?;
    assert_eq!(
        String::from_utf8(bytes.clone()).map_err(Box::<dyn Error>::from)?,
        "{\"v\":1,\"session\":\"s1\",\"controller\":\"run\",\"next_goal\":4,\"goal\":{\"id\":\"g3\",\"objective\":\"...\",\"status\":\"active\",\"created_at\":\"2026-09-25T10:15:30.123Z\",\"updated_at\":\"2026-09-25T10:20:00.000Z\",\"tokens_used\":12345,\"time_used_s\":300,\"consecutive\":0,\"unattended\":0,\"length_recoveries\":0,\"toolless_streak\":0,\"goal_turns\":0,\"last_signature\":null,\"recent_hashes\":[],\"blocked\":null,\"completed_at\":null}}\n",
    );
    let back = decode_sidecar(&bytes, "s1")?;
    assert_eq!(encode_sidecar(&back)?, bytes);
    assert_eq!(back.next_goal, 4);
    Ok(())
}

#[test]
fn decode_rejects_bad_documents_with_exact_texts() -> Result<(), Box<dyn Error>> {
    let session_mismatch =
        b"{\"v\":1,\"session\":\"other\",\"controller\":\"run\",\"next_goal\":1,\"goal\":null}\n";
    assert_eq!(
        decode_sidecar(session_mismatch, "s1")
            .unwrap_err()
            .to_string(),
        "goal: the goal file belongs to session other, not this one. dalgona ignores it."
    );
    let version =
        b"{\"v\":2,\"session\":\"s1\",\"controller\":\"run\",\"next_goal\":1,\"goal\":null}\n";
    assert_eq!(
        decode_sidecar(version, "s1").unwrap_err().to_string(),
        "goal: the goal file has version 2; this dalgona reads version 1."
    );
    for damaged in [
        "not json",
        "[1,2]",
        "{\"v\":1}",
        "{\"v\":1,\"session\":\"s1\",\"controller\":\"run\",\"next_goal\":1,\"goal\":null,\"x\":1}",
        "{\"v\":1,\"session\":\"s1\",\"controller\":\"run\",\"next_goal\":1,\"goal\":{\"id\":\"g1\",\"objective\":\"o\",\"status\":\"done\",\"created_at\":\"2026-09-25T10:15:30.123Z\",\"updated_at\":\"2026-09-25T10:15:30.123Z\",\"tokens_used\":0,\"time_used_s\":0,\"consecutive\":0,\"unattended\":0,\"length_recoveries\":0,\"toolless_streak\":0,\"goal_turns\":0,\"last_signature\":null,\"recent_hashes\":[],\"blocked\":null,\"completed_at\":null}}",
        "{\"v\":1,\"session\":\"s1\",\"controller\":\"run\",\"next_goal\":1,\"goal\":{\"id\":\"g1\",\"objective\":\"o\",\"status\":\"active\",\"created_at\":\"yesterday\",\"updated_at\":\"2026-09-25T10:15:30.123Z\",\"tokens_used\":0,\"time_used_s\":0,\"consecutive\":0,\"unattended\":0,\"length_recoveries\":0,\"toolless_streak\":0,\"goal_turns\":0,\"last_signature\":null,\"recent_hashes\":[],\"blocked\":null,\"completed_at\":null}}",
    ] {
        let error = decode_sidecar(damaged.as_bytes(), "s1")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("goal: the goal file is damaged: "),
            "unexpected damage text: {error}"
        );
    }
    Ok(())
}

#[test]
fn scope_gates_refuse_ephemeral_child_and_missing_sidecars() {
    let sidecar = empty_sidecar("s1");
    let ephemeral = GoalScope {
        session: "s1",
        saved: false,
        depth: 0,
    };
    assert_eq!(
        get_goal(&sidecar, &ephemeral).unwrap_err().to_string(),
        "goal: this session is not saved, so it cannot hold a goal. Start a saved session to use goals."
    );
    let child = GoalScope {
        session: "s1",
        saved: true,
        depth: 1,
    };
    assert_eq!(
        get_goal(&sidecar, &child).unwrap_err().to_string(),
        "goal: a subagent cannot hold a goal. Report to the agent that started you."
    );
    assert_eq!(
        get_goal(&sidecar, &scope("s1")).unwrap_or_default(),
        "No goal."
    );
}

#[test]
fn create_goal_reports_exact_texts() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, ctx) = active_sidecar()?;
    assert_eq!(
        create_goal(&mut sidecar, &ctx, "again", ts("2026-09-25T10:15:30.123Z")?)
            .unwrap_err()
            .to_string(),
        "create_goal: this session already has an unfinished goal (g1, active). Use update_goal when it is complete."
    );
    let long = "x".repeat(4001);
    assert_eq!(
        create_goal(
            &mut empty_sidecar("s1"),
            &ctx,
            &long,
            ts("2026-09-25T10:15:30.123Z")?
        )
        .unwrap_err()
        .to_string(),
        "create_goal: the objective has 4001 characters; the limit is 4000. Put the full objective in a file and name the file in the objective."
    );
    assert_eq!(
        create_goal(
            &mut empty_sidecar("s1"),
            &ctx,
            "",
            ts("2026-09-25T10:15:30.123Z")?
        )
        .unwrap_err()
        .to_string(),
        "create_goal: the objective is empty."
    );
    let reply = create_goal(
        &mut empty_sidecar("s1"),
        &ctx,
        "0123456789",
        ts("2026-09-25T10:15:30.123Z")?,
    )?;
    assert!(reply.starts_with("goal g1: active\nobjective: 0123456789\ntime: 0.0s · tokens: 0"));
    Ok(())
}

#[test]
fn update_goal_reports_exact_ordered_errors() -> Result<(), Box<dyn Error>> {
    let now = ts("2026-09-25T10:15:30.123Z")?;
    let (mut sidecar, ctx) = active_sidecar()?;
    let todos = TodoSummary {
        open: 0,
        total: 0,
        first_titles: Vec::new(),
    };
    let quiet = InflightCounts::default();
    assert_eq!(
        update_goal(
            &mut empty_sidecar("s1"),
            &ctx,
            UpdateTarget::Complete,
            None,
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: no goal in this session."
    );
    sidecar.goal.as_mut().ok_or("goal missing")?.status = GoalStatus::Paused;
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Complete,
            None,
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: the goal is paused, not active."
    );
    sidecar.goal.as_mut().ok_or("goal missing")?.status = GoalStatus::Active;
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            None,
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: reason is required when status is blocked."
    );
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            Some("   "),
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: reason is required when status is blocked."
    );
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Complete,
            Some("done-ish"),
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: reason must not be given when status is complete."
    );
    let busy_todos = TodoSummary {
        open: 2,
        total: 3,
        first_titles: vec!["parse".into(), "test".into()],
    };
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Complete,
            None,
            &busy_todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: 2 todo tasks are still open: parse; test."
    );
    let busy = inflight_counts(1, 0, 0, false, false);
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            Some("need a fact"),
            &todos,
            &busy,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: blocked is rejected while 1 job can still deliver. End the turn and let them wake you."
    );
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            Some("need a fact"),
            &todos,
            &quiet,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: blocked is rejected until the goal has had 3 goal turns since it became active or the user last spoke; it has had 0."
    );
    Ok(())
}

#[test]
fn blocked_parts_list_status_order_and_asks_alone() -> Result<(), Box<dyn Error>> {
    let now = ts("2026-09-25T10:15:30.123Z")?;
    let (mut sidecar, ctx) = active_sidecar()?;
    let todos = TodoSummary {
        open: 0,
        total: 0,
        first_titles: Vec::new(),
    };
    let mixed = inflight_counts(2, 1, 0, true, true);
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            Some("x"),
            &todos,
            &mixed,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: blocked is rejected while 2 jobs · 1 monitor · goal · loop guard can still deliver. End the turn and let them wake you."
    );
    let asks_only = inflight_counts(0, 0, 3, false, false);
    assert_eq!(
        update_goal(
            &mut sidecar,
            &ctx,
            UpdateTarget::Blocked,
            Some("x"),
            &todos,
            &asks_only,
            now
        )
        .unwrap_err()
        .to_string(),
        "update_goal: blocked is rejected while asks can still deliver. End the turn and let them wake you."
    );
    Ok(())
}

#[test]
fn blocked_success_is_nonmechanical_with_reason_line() -> Result<(), Box<dyn Error>> {
    let now = ts("2026-09-25T10:15:30.123Z")?;
    let (mut sidecar, ctx) = active_sidecar()?;
    sidecar.goal.as_mut().ok_or("goal missing")?.goal_turns = 3;
    let todos = TodoSummary {
        open: 0,
        total: 0,
        first_titles: Vec::new(),
    };
    let reply = update_goal(
        &mut sidecar,
        &ctx,
        UpdateTarget::Blocked,
        Some("waiting on the user"),
        &todos,
        &InflightCounts::default(),
        now,
    )?;
    assert!(reply.contains("goal g1: blocked"));
    assert!(reply.contains("blocked: waiting on the user"));
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(goal.status, GoalStatus::Blocked);
    assert_eq!(
        goal.blocked.as_ref().map(|blocked| blocked.mechanical),
        Some(false)
    );
    Ok(())
}

#[test]
fn goal_commands_reply_exactly() -> Result<(), Box<dyn Error>> {
    let now = ts("2026-09-25T10:15:30.123Z")?;
    let mut sidecar = empty_sidecar("s1");
    let ctx = scope("s1");
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Show, now),
        "No goal."
    );
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Clear, now),
        "No goal."
    );
    let created = apply_goal_command(
        &mut sidecar,
        &ctx,
        &parse_goal_command("write the parser"),
        now,
    );
    assert!(created.starts_with("goal g1: active"));
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Pause, now),
        "goal g1 paused."
    );
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Resume, now),
        "goal g1 is active."
    );
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Clear, now),
        "goal g1 cleared."
    );
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Show, now),
        "No goal."
    );
    Ok(())
}

#[test]
fn pause_on_complete_reports_the_update_error() -> Result<(), Box<dyn Error>> {
    let now = ts("2026-09-25T10:15:30.123Z")?;
    let (mut sidecar, ctx) = active_sidecar()?;
    let todos = TodoSummary {
        open: 0,
        total: 0,
        first_titles: Vec::new(),
    };
    update_goal(
        &mut sidecar,
        &ctx,
        UpdateTarget::Complete,
        None,
        &todos,
        &InflightCounts::default(),
        now,
    )?;
    assert_eq!(
        apply_goal_command(&mut sidecar, &ctx, &GoalCommand::Pause, now),
        "update_goal: the goal is complete, not active."
    );
    Ok(())
}

#[test]
fn verdict_denies_in_plan_order() -> Result<(), Box<dyn Error>> {
    let (sidecar, _) = active_sidecar()?;
    let quiet = InflightCounts::default();
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(
        verdict(&base_input(goal, &quiet)),
        Verdict::Continue {
            prompt: PromptKind::Full,
            stall: false,
        }
    );
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.last_turn_context_overflow = true;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::ContextOverflow));
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.pending_user_messages = true;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::NotEligible));
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.continuation_pending = true;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::SingleFlight));
    Ok(())
}

#[test]
fn verdict_tracks_repetition_unattended_and_cap() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, _) = active_sidecar()?;
    let quiet = InflightCounts::default();
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.recent_hashes = vec!["h".into(), "h".into(), "h".into()];
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(
        verdict(&base_input(goal, &quiet)),
        Verdict::Deny(DenyReason::Repetition)
    );
    assert_eq!(
        DenyReason::Repetition.mechanical_reason(),
        Some("repeated assistant output")
    );
    assert_eq!(DenyReason::Stale.mechanical_reason(), None);
    assert_eq!(DenyReason::SingleFlight.mechanical_reason(), None);
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.recent_hashes.clear();
        goal.unattended = 150;
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.path = GoalPath::Idle;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::Unattended));
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.unattended = 0;
        goal.consecutive = 8;
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.path = GoalPath::Recovery;
    input.idle = false;
    input.last_stop = StopKind::Error;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::Cap));
    Ok(())
}

#[test]
fn stale_returns_without_state_and_length_recovers_once() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, _) = active_sidecar()?;
    let quiet = InflightCounts::default();
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.last_signature = Some("g1:0/0:abcd1234".into());
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(
        verdict(&base_input(goal, &quiet)),
        Verdict::Deny(DenyReason::Stale)
    );
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.last_signature = None;
        goal.toolless_streak = 3;
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.path = GoalPath::Idle;
    input.last_stop = StopKind::Length;
    assert_eq!(
        verdict(&input),
        Verdict::Continue {
            prompt: PromptKind::Minimal,
            stall: true,
        }
    );
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.length_recoveries = 1;
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let mut input = base_input(goal, &quiet);
    input.path = GoalPath::Idle;
    input.last_stop = StopKind::Length;
    assert_eq!(verdict(&input), Verdict::Deny(DenyReason::LengthExhausted));
    Ok(())
}

#[test]
fn goal_turn_accounting_resets_consecutive_on_signature_change() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, _) = active_sidecar()?;
    for turn in 0..8 {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        record_goal_turn(
            goal,
            &format!("output {turn}"),
            true,
            10,
            5,
            "g1:0/0:same",
            PromptKind::Full,
        );
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(goal.consecutive, 8);
    assert_eq!(goal.goal_turns, 8);
    assert_eq!(goal.tokens_used, 80);
    assert_eq!(goal.time_used_s, 40);
    assert_eq!(goal.toolless_streak, 0);
    assert_eq!(goal.recent_hashes.len(), 3);
    let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
    record_goal_turn(
        goal,
        "new work",
        false,
        1,
        1,
        "g1:0/0:changed",
        PromptKind::Full,
    );
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(goal.consecutive, 1);
    assert_eq!(goal.toolless_streak, 1);
    Ok(())
}

#[test]
fn user_prompt_reactivates_only_mechanical_blocks() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, _) = active_sidecar()?;
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.status = GoalStatus::Blocked;
        goal.blocked = Some(BlockedReason {
            reason: "repeated assistant output".into(),
            at: ts("2026-09-25T10:15:30.123Z")?,
            mechanical: true,
        });
        goal.consecutive = 5;
    }
    on_user_prompt(sidecar.goal.as_mut().ok_or("goal missing")?);
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.blocked, None);
    assert_eq!(goal.consecutive, 0);
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.status = GoalStatus::Blocked;
        goal.blocked = Some(BlockedReason {
            reason: "waiting on the user".into(),
            at: ts("2026-09-25T10:15:30.123Z")?,
            mechanical: false,
        });
    }
    on_user_prompt(sidecar.goal.as_mut().ok_or("goal missing")?);
    assert_eq!(
        sidecar.goal.as_ref().ok_or("goal missing")?.status,
        GoalStatus::Blocked
    );
    Ok(())
}

#[test]
fn progress_signatures_normalize_text_before_hashing() {
    let plain = progress_signature("g1", 1, 2, "Fix the parser");
    let other_case = progress_signature("g1", 1, 2, "fix THE parser");
    let spaced = progress_signature("g1", 1, 2, "  fix\t the\n parser ");
    assert_eq!(plain, other_case);
    assert_eq!(plain, spaced);
    assert!(plain.starts_with("g1:1/2:"));
    assert_eq!(plain.len(), 15);
    let hash: String = plain
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    assert!(
        hash.bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    );
}

#[test]
fn prompts_escape_in_order_and_stall_after_three_toolless_turns() -> Result<(), Box<dyn Error>> {
    let (mut sidecar, _) = active_sidecar()?;
    {
        let goal = sidecar.goal.as_mut().ok_or("goal missing")?;
        goal.objective = "use & check <fast> paths".into();
        goal.tokens_used = 7;
        goal.time_used_s = 9;
        goal.toolless_streak = 3;
    }
    let goal = sidecar.goal.as_ref().ok_or("goal missing")?;
    let full = build_prompt(goal, PromptKind::Full, 4, &["1 job".into()]);
    assert!(full.contains("use &amp; check &lt;fast&gt; paths"));
    assert!(full.contains("Usage so far: 9 seconds, 7 tokens."));
    assert!(full.contains("goal continuation #4 in a row, and the live channels (1 job)"));
    let dead = build_prompt(goal, PromptKind::Full, 4, &[]);
    assert!(dead.contains("with no tool use and no new user input"));
    let minimal = build_prompt(goal, PromptKind::Minimal, 2, &[]);
    assert!(minimal.starts_with("Your previous response was cut off"));
    assert_eq!(escape_objective("a&b<c>d"), "a&amp;b&lt;c&gt;d");
    Ok(())
}

#[test]
fn durations_continuations_and_recovery_shape() {
    assert_eq!(format_duration(48), "48.0s");
    assert_eq!(format_duration(252), "4m12s");
    assert_eq!(
        continuation_line(&ControllerMode::Run),
        "automatic turns: run"
    );
    assert_eq!(
        continuation_line(&ControllerMode::Paused {
            reason: "paused by the user"
        }),
        "automatic turns: paused (paused by the user)"
    );
    assert_eq!(
        continuation_line(&ControllerMode::Stopped),
        "automatic turns: stopped. Only /continuation run starts them again."
    );
    assert_eq!(
        continuation_unknown(),
        "continuation: use run, pause, or stop."
    );
    assert_eq!(
        continuation_unsaved("automatic turns: stopped.", "denied"),
        "automatic turns: stopped. (not saved: denied)"
    );
    assert_eq!(salvage_next_goal(b"{\"next_goal\":7}"), Some(7));
    assert_eq!(salvage_next_goal(b"broken"), None);
    let doc = clear_recovery_doc("s9", ControllerMode::Stopped, 3);
    assert!(doc.ends_with(b"\n"));
    assert!(String::from_utf8_lossy(&doc).contains("\"controller\":\"stopped\""));
}
