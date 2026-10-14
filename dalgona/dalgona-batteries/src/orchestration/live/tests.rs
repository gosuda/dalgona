// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Tests for the read-only view: one handle serves every session, readers see
//! one atomic publish, and close removes the session.

use dal_core::SessionId;

use super::super::ControllerMode;
use super::super::monitor::status::{StatusPayload, inflight_counts};
use super::{Cell, GoalView, InflightCounts, Published, View};

fn published(jobs: usize) -> Published {
    let inflight = inflight_counts(jobs, 0, 0, false, false);
    Published {
        inflight: InflightCounts {
            jobs: inflight.jobs,
            monitors: inflight.monitors,
            asks: inflight.asks,
            goal_timer: inflight.goal_timer > 0,
            loop_guard: inflight.loop_guard > 0,
        },
        goal: None,
        status: StatusPayload::new(ControllerMode::Run, false, inflight, 0, None),
    }
}

#[test]
fn inflight_counts_report_live_work() {
    let quiet = InflightCounts::default();
    assert!(!quiet.is_live());
    for live in [
        InflightCounts {
            jobs: 1,
            ..InflightCounts::default()
        },
        InflightCounts {
            monitors: 2,
            ..InflightCounts::default()
        },
        InflightCounts {
            asks: 1,
            ..InflightCounts::default()
        },
        InflightCounts {
            goal_timer: true,
            ..InflightCounts::default()
        },
        InflightCounts {
            loop_guard: true,
            ..InflightCounts::default()
        },
    ] {
        assert!(live.is_live(), "{live:?} counts as live work");
    }
}

#[test]
fn view_publishes_and_clears_per_session() -> Result<(), &'static str> {
    let view = View::default();
    let session = SessionId::new_v7();
    assert_eq!(view.inflight(session), None);
    assert_eq!(view.goal(session), None);
    assert_eq!(view.status(session), None);

    let cell = view.open(session);
    assert_eq!(view.inflight(session), None, "open publishes nothing yet");
    cell.publish(published(2));
    let counts = view.inflight(session).ok_or("counts published")?;
    assert_eq!(counts.jobs, 2);
    assert!(counts.is_live());

    view.close(session);
    assert_eq!(view.inflight(session), None, "close clears the projection");
    assert_eq!(view.goal(session), None, "close clears the goal");
    assert_eq!(view.status(session), None, "close clears the status");
    Ok(())
}

#[test]
fn view_keeps_sessions_independent() {
    let view = View::default();
    let first = view.open(SessionId::new_v7());
    let second_session = SessionId::new_v7();
    let second = view.open(second_session);
    first.publish(published(1));
    second.publish(published(0));
    assert_eq!(
        view.inflight(second_session).map(|counts| counts.jobs),
        Some(0)
    );
}

#[test]
fn goal_view_survives_a_publish_cycle() {
    let view = View::default();
    let session = SessionId::new_v7();
    let cell: Cell = view.open(session);
    let mut state = published(0);
    state.goal = Some(GoalView {
        id: "g1".into(),
        status: "active".into(),
        objective: "write the parser".into(),
    });
    cell.publish(state);
    let goal = view.goal(session).unwrap();
    assert_eq!(goal.id, "g1");
    assert_eq!(goal.status, "active");
    assert_eq!(goal.objective, "write the parser");
}

#[test]
fn clones_read_the_same_sessions() {
    let view = View::default();
    let session = SessionId::new_v7();
    let cell = view.open(session);
    cell.publish(published(3));
    let reader = view.clone();
    assert_eq!(reader.inflight(session).map(|counts| counts.jobs), Some(3));
    view.close(session);
    assert_eq!(reader.inflight(session), None);
}

#[test]
fn work_summary_names_live_work_and_ignores_asks() {
    assert_eq!(InflightCounts::default().work_summary(), None);
    let asks_only = InflightCounts {
        asks: 3,
        ..InflightCounts::default()
    };
    assert_eq!(asks_only.work_summary(), None, "an open ask is not work");
    assert!(asks_only.is_live(), "an open ask still counts as live");

    let one = InflightCounts {
        jobs: 1,
        monitors: 1,
        ..InflightCounts::default()
    };
    assert_eq!(one.work_summary().as_deref(), Some("1 job, 1 monitor"));
    let many = InflightCounts {
        jobs: 2,
        monitors: 3,
        goal_timer: true,
        loop_guard: true,
        ..InflightCounts::default()
    };
    assert_eq!(
        many.work_summary().as_deref(),
        Some("2 jobs, 3 monitors, a scheduled goal continuation, a loop-guard recovery")
    );
}
