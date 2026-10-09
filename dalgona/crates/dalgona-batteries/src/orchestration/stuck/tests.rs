// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! State-machine tests for the guard, sleep, and rewrite reducers.

use std::error::Error;

use dal_core::{RawJson, Timestamp};
use sonic_rs::{JsonValueTrait, Value};

use super::LOOP_HARD_STOP_REASON;
use super::guard::{GuardEffects, GuardState, GuardVerdict, on_tool_call, reset};
use super::rewrite::rewrite_exec_args;
use super::sleep::{SleepClassifier, SleepRule, SleepWait};

fn raw(value: &str) -> Result<RawJson, Box<dyn Error>> {
    Ok(RawJson::parse(value)?)
}

#[test]
fn identical_calls_arm_then_block_and_hard_stop_once() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let args = raw(r#"{"path":"a"}"#)?;
    let calls = (1..=9)
        .map(|_| on_tool_call(&mut state, "read", &args))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(matches!(calls[5].verdict, GuardVerdict::Allow));
    assert!(matches!(calls[6].verdict, GuardVerdict::Block { .. }));
    assert!(matches!(calls[7].verdict, GuardVerdict::Block { .. }));
    assert_eq!(
        calls[8].warning.as_deref(),
        Some("Loop guard interrupted the turn after blocking 3 repeated calls to read.")
    );
    assert_eq!(
        calls[8].p1_recovery.as_deref(),
        Some(
            "The loop guard stopped the previous turn because you kept calling `read` with arguments that had already been blocked. Do not repeat that call. Re-plan from the current goal and use a different tool or deliberately changed arguments."
        )
    );
    assert_eq!(calls[8].pause_reason, None);
    Ok(())
}

#[test]
fn second_hard_stop_pauses_without_a_second_warning() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let args = raw("{}")?;
    for _ in 0..9 {
        on_tool_call(&mut state, "read", &args)?;
    }
    let effects = on_tool_call(&mut state, "read", &args)?;
    assert!(effects.cancel_turn);
    assert_eq!(effects.warning, None);
    assert_eq!(effects.p1_recovery, None);
    assert_eq!(effects.pause_reason, Some(LOOP_HARD_STOP_REASON));
    Ok(())
}

#[test]
fn a_fresh_signature_ends_the_guard_episode() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let first = raw(r#"{"path":"a"}"#)?;
    let other = raw(r#"{"path":"b"}"#)?;
    for _ in 0..6 {
        on_tool_call(&mut state, "read", &first)?;
    }
    let reset = on_tool_call(&mut state, "read", &other)?;
    assert_eq!(reset.verdict, GuardVerdict::Allow);
    let old_again = on_tool_call(&mut state, "read", &first)?;
    assert_eq!(old_again.verdict, GuardVerdict::Allow);
    Ok(())
}

#[test]
fn person_reset_clears_records_gates_episode_and_pending_attempts() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let args = raw("{}")?;
    for _ in 0..6 {
        on_tool_call(&mut state, "read", &args)?;
    }
    reset(&mut state);
    assert!(state.records.is_empty());
    assert!(state.gates.is_empty());
    assert!(state.episode.is_none());
    assert!(state.pending_attempts.is_empty());
    Ok(())
}

#[test]
fn cycle_detector_reports_period_two() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let read = raw(r#"{"path":"a"}"#)?;
    let search = raw(r#"{"query":"b"}"#)?;
    let mut last = GuardEffects::allow();
    for index in 0..6 {
        last = if index % 2 == 0 {
            on_tool_call(&mut state, "read", &read)?
        } else {
            on_tool_call(&mut state, "search", &search)?
        };
    }
    let notice = last.steer.ok_or("cycle notice missing")?;
    assert!(notice.contains("[read -> search] 3 times (period 2)"));
    Ok(())
}

#[test]
fn similar_detector_uses_mean_adjacent_dice_score() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let mut last = GuardEffects::allow();
    for index in 1..=5 {
        let args = raw(&format!(r#"{{"query":"foo{index}"}}"#))?;
        last = on_tool_call(&mut state, "search", &args)?;
    }
    let notice = last.steer.ok_or("similar notice missing")?;
    assert!(notice.contains("LOOP GUARD - NEAR-IDENTICAL TOOL CALLS"));
    assert!(notice.contains("87% identical"));
    Ok(())
}

#[test]
fn read_similarity_does_not_fire_for_five_distinct_targets() -> Result<(), Box<dyn Error>> {
    let mut state = GuardState::default();
    let mut last = GuardEffects::allow();
    for index in 0..5 {
        let args = raw(&format!(r#"{{"path":"src/file{index}.rs"}}"#))?;
        last = on_tool_call(&mut state, "read", &args)?;
    }
    assert_eq!(last.steer, None);
    Ok(())
}

#[test]
fn sleep_classifier_follows_rules_and_shell_wrappers() -> Result<(), Box<dyn Error>> {
    let classifier = SleepClassifier::new()?;
    let cases = [
        ("sleep 30", Some((SleepRule::R1, 30.0))),
        ("sleep 12; git log", Some((SleepRule::R2, 12.0))),
        (
            "while true; do curl x; sleep 5; done",
            Some((SleepRule::R3, 5.0)),
        ),
        ("make && sleep 20", Some((SleepRule::R4, 20.0))),
        ("bash -lc 'sleep 15'", Some((SleepRule::R1, 15.0))),
        ("env A=1 sh -c \"sleep 11\"", Some((SleepRule::R1, 11.0))),
        ("pkill x; sleep 1", None),
        ("caffeinate -t 3600 sleep 100", None),
        ("./sleepless 300", None),
        ("--sleep=30", None),
        ("for f in *; do echo; sleep 1; done", None),
        ("sleep 9", None),
    ];
    for (command, expected) in cases {
        assert_eq!(
            classifier
                .classify(command)
                .map(|wait| (wait.rule, wait.seconds)),
            expected,
            "classification for {command:?}"
        );
    }
    Ok(())
}

#[test]
fn shell_classifier_peels_no_more_than_three_wrappers() -> Result<(), Box<dyn Error>> {
    let classifier = SleepClassifier::new()?;
    let mut three = String::from("sleep 12");
    for _ in 0..3 {
        three = format!("sh -c '{three}'");
    }
    let mut four = format!("sh -c '{three}'");
    assert_eq!(
        classifier.classify(&three).map(|wait| wait.seconds),
        Some(12.0)
    );
    assert_eq!(classifier.classify(&four), None);
    assert_eq!(classifier.classify("sleep 30 && pmset sleepnow"), None);
    four.clear();
    Ok(())
}

#[test]
fn rewrite_adds_and_clamps_foreground_window_to_five_seconds() -> Result<(), Box<dyn Error>> {
    let wait = SleepWait {
        rule: SleepRule::R1,
        seconds: 30.0,
    };
    for args in [
        raw(r#"{"command":"sleep 30"}"#)?,
        raw(r#"{"command":"sleep 30","foreground_s":60}"#)?,
    ] {
        let rewritten = rewrite_exec_args(&args, wait)?.ok_or("rewrite missing")?;
        let value: Value = sonic_rs::from_str(rewritten.as_str())?;
        assert_eq!(
            value
                .get("foreground_s")
                .and_then(|seconds| seconds.as_u64()),
            Some(5)
        );
    }
    Ok(())
}

#[test]
fn rewrite_preserves_a_short_existing_window() -> Result<(), Box<dyn Error>> {
    let wait = SleepWait {
        rule: SleepRule::R1,
        seconds: 30.0,
    };
    let args = raw(r#"{"command":"sleep 30","foreground_s":2}"#)?;
    assert_eq!(rewrite_exec_args(&args, wait)?, None);
    Ok(())
}
