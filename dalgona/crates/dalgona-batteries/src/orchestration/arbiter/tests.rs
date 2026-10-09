// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Arbiter tests: priority, budget, release, modes, exactly-once delivery.

use super::*;
use std::time::Duration;

fn now() -> Instant {
    Instant::now()
}

fn report(from_run: bool) -> JobReport {
    JobReport {
        id: JobId::new_v7(),
        text: "x".repeat(1000),
        from_run,
    }
}

#[test]
fn arbiter_priority_and_budget() {
    let arbiter = Arbiter::new();
    let mut arbiter = arbiter;
    let at = now();
    arbiter.admit_recovery("recover".to_owned(), at);
    arbiter.push_monitor("m1".to_owned(), at);
    arbiter.admit_goal("goal".to_owned(), at);
    let reports = vec![report(true), report(false)];
    let ids: Vec<JobId> = reports.iter().map(|report| report.id).collect();
    let items = arbiter.collect(reports);
    let (text, sources, included) = arbiter.compose(&items, INJECTION_BUDGET);
    assert_eq!(sources, ["loop_guard", "jobs", "monitor"]);
    assert_eq!(included, ids);
    assert!(text.contains("recover"));
    assert!(!text.starts_with('\n'));
    assert!(text.contains(super::CLAIM_HONESTY));
    assert_eq!(text.matches(super::CLAIM_HONESTY).count(), 1);
    let later = arbiter.collect(Vec::new());
    assert_eq!(later.len(), 1);
    assert!(matches!(later[0], Ready::Goal(_)));
}

#[test]
fn arbiter_monitor_fit_matches_compose() {
    let arbiter = Arbiter::new();
    let batches = vec!["a".repeat(100), "b".repeat(100), "c".repeat(100)];
    let take = Arbiter::fit_monitor(&batches, 0, 250);
    assert_eq!(take, 2);
    let items = vec![Ready::Monitor(batches)];
    let (text, sources, _) = arbiter.compose(&items, 250);
    assert_eq!(sources, ["monitor"]);
    assert!(text.contains(&"a".repeat(100)));
    assert!(!text.contains(&"c".repeat(100)));
}

#[test]
fn arbiter_budget_leaves_overflow_for_next_wake() {
    let arbiter = Arbiter::new();
    let reports: Vec<JobReport> = (0..30).map(|_| report(false)).collect();
    let items = vec![Ready::Jobs(reports)];
    let (first, _, included) = arbiter.compose(&items, INJECTION_BUDGET);
    assert!(first.len() <= INJECTION_BUDGET);
    assert!(!included.is_empty() && included.len() < 30);
}

#[test]
fn arbiter_busy_releases() {
    let mut arbiter = Arbiter::new();
    let id = JobId::new_v7();
    arbiter.commit(std::slice::from_ref(&id));
    assert!(arbiter.is_committed(&id));
    arbiter.release(std::slice::from_ref(&id));
    assert!(!arbiter.is_committed(&id));
    let items = vec![Ready::Jobs(vec![JobReport {
        id,
        text: "late".to_owned(),
        from_run: false,
    }])];
    let (_, _, included) = arbiter.compose(&items, INJECTION_BUDGET);
    assert_eq!(included, [id]);
}

#[test]
fn arbiter_limit_pauses() {
    let mut arbiter = Arbiter::new();
    arbiter.on_continuation_run();
    assert_eq!(arbiter.mode(), ControllerMode::Run);
    arbiter.stop();
    assert_eq!(arbiter.mode(), ControllerMode::Stopped);
    arbiter.on_user_prompt();
    assert_eq!(arbiter.mode(), ControllerMode::Stopped);
    arbiter.on_continuation_run();
    assert_eq!(arbiter.mode(), ControllerMode::Run);
}

#[test]
fn arbiter_quiet_needs_idle_and_grace() {
    let arbiter = Arbiter::new();
    let at = now();
    assert!(!arbiter.quiet(false, 0, 0, at));
    assert!(!arbiter.quiet(true, 1, 0, at));
    assert!(!arbiter.quiet(true, 0, 1, at));
    assert!(arbiter.quiet(true, 0, 0, at + QUIET_AFTER + Duration::from_millis(1)));
}

#[test]
fn arbiter_exactly_once_model() {
    let mut seed = 0x243F_6A88_85A3_08D3_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        seed
    };
    let base = now();
    for round in 0..1000 {
        let mut arbiter = Arbiter::new();
        arbiter.on_continuation_run();
        let mut ended: Vec<JobId> = (0..8).map(|_| JobId::new_v7()).collect();
        let mut delivered: Vec<JobId> = Vec::new();
        let at = base + Duration::from_millis(round);
        for step in 0..24 {
            let roll = next() % 5;
            if roll == 0 && !ended.is_empty() {
                let len = u64::try_from(ended.len()).expect("small pool");
                let pick = usize::try_from(next() % len).expect("index fits");
                let id = ended.remove(pick);
                let items = vec![Ready::Jobs(vec![JobReport {
                    id,
                    text: "report".to_owned(),
                    from_run: next() % 2 == 0,
                }])];
                let mode = arbiter.mode();
                if mode == ControllerMode::Run {
                    let (_, _, included) = arbiter.compose(&items, INJECTION_BUDGET);
                    if next() % 4 == 0 {
                        arbiter.release(&included);
                    } else {
                        arbiter.commit(&included);
                        delivered.extend(included);
                    }
                }
            }
            let _ = (step, at);
        }
        let mut delivered_ids = HashSet::new();
        for id in &delivered {
            assert!(delivered_ids.insert(id), "job delivered twice");
            assert!(arbiter.is_committed(id));
        }
    }
}
