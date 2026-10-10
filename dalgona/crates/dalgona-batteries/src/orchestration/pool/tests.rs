// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pool index-ordering tests: the last item may finish first.

use super::*;
use crate::orchestration::agents_tool::{ReportCell, ReportStatus, submit};

fn result(state: TaskState) -> TaskResult {
    TaskResult {
        id: JobId::new_v7(),
        state,
        changed: Vec::new(),
        isolation: None,
        body: "".into(),
        item: "".into(),
    }
}

#[test]
fn pool_fifo_result_index() {
    let mut collector = IndexCollector::new(3);
    assert!(!collector.is_complete());
    collector.insert(2, result(TaskState::Cancelled));
    collector.insert(0, result(TaskState::Failed("boom".to_owned())));
    assert_eq!(collector.into_ordered().len(), 0);
    let mut collector = IndexCollector::new(3);
    collector.insert(2, result(TaskState::Cancelled));
    collector.insert(0, result(TaskState::Failed("boom".to_owned())));
    let cell = ReportCell::default();
    submit(&cell, ReportStatus::Done, "all good");
    let report = cell.get().expect("report stored");
    collector.insert(1, result(TaskState::Done(report)));
    let ordered = collector.into_ordered();
    assert_eq!(
        ordered
            .iter()
            .map(|task| task.state.word())
            .collect::<Vec<_>>(),
        ["failed", "done", "cancelled"]
    );
}

#[test]
fn pool_state_words_cover_every_ending() {
    let cell = ReportCell::default();
    submit(&cell, ReportStatus::Blocked, "waiting");
    let report = cell.get().expect("report stored");
    let words = [
        TaskState::Done(report.clone()).word(),
        TaskState::Blocked(report).word(),
        TaskState::Failed(String::new()).word(),
        TaskState::Cancelled.word(),
        TaskState::Skipped(String::new()).word(),
    ];
    assert_eq!(words, ["done", "blocked", "failed", "cancelled", "skipped"]);
}

fn stored(status: ReportStatus, text: &str) -> Report {
    let cell = ReportCell::default();
    submit(&cell, status, text);
    cell.get().expect("report stored")
}

fn ended(report: Option<Report>, stop: StopReason) -> ChildEnd {
    ChildEnd {
        report,
        stop,
        deadline_hit: false,
        max_rounds: 50,
        max_minutes: 30,
        last_text: "tried hard".to_owned(),
    }
}

#[test]
fn pool_first_result_wins_over_late_report() {
    let mut collector = IndexCollector::new(1);
    collector.insert(0, result(TaskState::Failed("first".to_owned())));
    collector.insert(
        0,
        result(TaskState::Done(stored(ReportStatus::Done, "late"))),
    );
    let ordered = collector.into_ordered();
    assert_eq!(ordered.len(), 1);
    assert_eq!(ordered[0].state.word(), "failed");
}

#[test]
fn child_grace_endings() {
    // (a) A report settles at once with no grace.
    let ChildDecision::Settle(settled) = decide(&ended(
        Some(stored(ReportStatus::Done, "all good")),
        StopReason::EndTurn,
    )) else {
        panic!("a report settles");
    };
    assert_eq!(settled.state.word(), "done");
    assert_eq!(settled.note, None);
    // (b) Silence then a grace report settles with the note.
    let ChildDecision::Grace(cause) = decide(&ended(None, StopReason::EndTurn)) else {
        panic!("silence earns grace");
    };
    assert_eq!(cause, GraceCause::NoReport);
    let late = decide_grace(
        Some(&stored(ReportStatus::Done, "late but complete")),
        cause,
    );
    assert_eq!(late.state.word(), "done");
    assert_eq!(late.note.as_deref(), Some("reported after no report"));
    // (c) Silence through grace fails and shows the last message.
    let silent = decide_grace(None, GraceCause::NoReport);
    assert_eq!(
        silent.state,
        TaskState::Failed("no report after the last turn".to_owned())
    );
    assert_eq!(
        last_message_text("tried hard"),
        "last message (not a report): tried hard"
    );
    // (d) Tool rounds earn grace with the round reason.
    let ChildDecision::Grace(cause) = decide(&ended(None, StopReason::MaxSteps)) else {
        panic!("max steps earns grace");
    };
    assert_eq!(
        grace_text(cause),
        format!(
            "You used all 50 tool rounds of this turn. You have one last turn. Call {} now with your best answer. Use status done only if the task is complete; otherwise use blocked or failed, and say in the report that your work was cut short. Do not call any other tool.",
            crate::orchestration::agents_tool::REPORT_TOOL_NAME
        )
    );
    // (e) A fired deadline earns grace with the time reason.
    let mut timed_out = ended(None, StopReason::Cancelled);
    timed_out.deadline_hit = true;
    let ChildDecision::Grace(cause) = decide(&timed_out) else {
        panic!("deadline earns grace");
    };
    assert!(grace_text(cause).starts_with("You reached the time limit of 30 minutes."));
    // (f) A provider error fails with its message and no grace.
    let ChildDecision::Settle(settled) = decide(&ended(None, StopReason::Error("boom".to_owned())))
    else {
        panic!("provider error settles");
    };
    assert_eq!(settled.state, TaskState::Failed("boom".to_owned()));
    // (g) A plain cancel settles cancelled with no grace.
    let ChildDecision::Settle(settled) = decide(&ended(None, StopReason::Cancelled)) else {
        panic!("cancel settles");
    };
    assert_eq!(settled.state, TaskState::Cancelled);
    // (h) A filtered reply fails with the fixed text.
    let ChildDecision::Settle(settled) = decide(&ended(None, StopReason::Filter)) else {
        panic!("filter settles");
    };
    assert_eq!(
        settled.state,
        TaskState::Failed("the provider filtered the reply".to_owned())
    );
}

#[test]
fn pool_text_builders_match_contract() {
    assert!(preamble("audit", "Check.").contains("Task \"audit\":\nCheck."));
    assert!(
        preamble("audit", "Check.").contains(crate::orchestration::agents_tool::REPORT_TOOL_NAME)
    );
    assert_eq!(item_label("audit", 0, "routes"), "audit 1: routes");
    assert_eq!(
        unresolved_skip("b"),
        TaskState::Skipped("step b produced no result".to_owned())
    );
    assert_eq!(no_items_skip("b").word(), "skipped");
    assert_eq!(
        too_many_items("b", 2000),
        "step b reported 2000 items; the limit is 1024"
    );
    assert_eq!(
        pool_budget_short("a", 5, 2),
        "step a needs 5 subagents, but the session has 2 left"
    );
    assert_eq!(unfinished_text(0, 3), None);
    assert_eq!(
        unfinished_text(2, 3).as_deref(),
        Some("2 of 3 tasks did not finish")
    );
    assert!((GRACE_SECONDS - 60.0).abs() < f64::EPSILON);
}

#[test]
fn last_message_cut_never_splits_a_character_at_the_byte_limit() {
    // 599 ASCII bytes, then a two-byte character straddling byte 600.
    let text = format!("{}é tail", "a".repeat(LAST_MESSAGE_LIMIT - 1));
    let shown = last_message_text(&text);
    let excerpt = shown
        .strip_prefix("last message (not a report): ")
        .expect("the excerpt keeps its prefix");
    assert_eq!(
        excerpt.len(),
        LAST_MESSAGE_LIMIT - 1,
        "the straddling char is dropped whole"
    );
    assert!(excerpt.chars().all(|c| c == 'a'));
    // A character that ends exactly on the limit stays.
    let text = format!("{}é", "a".repeat(LAST_MESSAGE_LIMIT - 2));
    let shown = last_message_text(&text);
    assert!(shown.ends_with('é'));
}
