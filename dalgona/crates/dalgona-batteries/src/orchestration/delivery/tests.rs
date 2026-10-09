// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Delivery tests: ordered counts, bounded notices, one honesty suffix.

use super::*;
use crate::orchestration::agents_tool::{ReportCell, ReportStatus, submit};
use crate::orchestration::pool::TaskState;

fn done_task(report: &str, changed: &[&str]) -> TaskResult {
    let cell = ReportCell::default();
    submit(&cell, ReportStatus::Done, report);
    TaskResult {
        id: JobId::new_v7(),
        state: TaskState::Done(cell.get().expect("report stored")),
        changed: changed.iter().map(PathBuf::from).collect(),
        isolation: None,
        body: report.into(),
        item: report.into(),
    }
}

fn notice<'a>(tasks: &'a [TaskNotice<'a>]) -> Vec<StepNotice<'a>> {
    vec![StepNotice {
        name: "build",
        tasks: tasks.to_vec(),
        pool: true,
        skipped: None,
    }]
}

#[test]
fn delivery_report_paths_from_records() {
    let first = done_task("first", &["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs"]);
    let failed = TaskResult {
        id: JobId::new_v7(),
        state: TaskState::Failed("boom".to_owned()),
        changed: Vec::new(),
        isolation: None,
        body: "boom".into(),
        item: "boom".into(),
    };
    let tasks = [
        TaskNotice {
            id: first.id,
            label: "build 1: a",
            state: &first.state,
            changed: &first.changed,
            preview_text: "first",
            suffix: "",
        },
        TaskNotice {
            id: failed.id,
            label: "build 2: b",
            state: &failed.state,
            changed: &failed.changed,
            preview_text: "boom",
            suffix: "",
        },
    ];
    let sections = notice(&tasks);
    let text = run_notice(
        JobId::new_v7(),
        "labels",
        "ended",
        "48.0s",
        &sections,
        RUN_NOTICE_LIMIT,
    );
    assert!(text.contains("2 tasks: 1 done, 1 failed"));
    assert!(text.contains("changed: src/a.rs, src/b.rs, src/c.rs (+1 more)"));
    assert!(text.contains("changed: none"));
    assert!(text.contains("step build: pool of 2 · 1 done, 1 failed"));
    assert!(text.contains("in 48.0s"));
    assert_eq!(text.matches(super::CLAIM_HONESTY).count(), 1);
    assert!(text.len() <= RUN_NOTICE_LIMIT);
    assert!(changed_paths_valid(&first.changed));
}

#[test]
fn delivery_claim_honesty_once() {
    let run = JobId::new_v7();
    let tasks: Vec<TaskResult> = (0..300)
        .map(|_| done_task(&"x".repeat(500), &["src/a.rs"]))
        .collect();
    let notices: Vec<TaskNotice> = tasks
        .iter()
        .map(|task| TaskNotice {
            id: task.id,
            label: "item",
            state: &task.state,
            changed: &task.changed,
            preview_text: "x",
            suffix: "",
        })
        .collect();
    let sections = notice(&notices);
    let text = run_notice(run, "big", "ended", "4m12s", &sections, RUN_NOTICE_LIMIT);
    assert!(text.len() <= RUN_NOTICE_LIMIT);
    assert!(text.ends_with(super::CLAIM_HONESTY));
    assert_eq!(text.matches(super::CLAIM_HONESTY).count(), 1);
    assert!(text.contains("more tasks: read job://"));
}

#[test]
fn delivery_preview_cut_is_utf8_safe() {
    let cut = preview(&"é".repeat(200), 100);
    assert!(cut.ends_with("..."));
    assert!(cut.is_char_boundary(cut.len()));
    assert_eq!(preview("short", 100), "short");
    assert_eq!(preview_limit(1), 1200);
    assert_eq!(preview_limit(100), 160);
    assert_eq!(preview_limit(0), 1200);
}

#[test]
fn delivery_counts_split_states_and_skip_words() {
    let states = [
        TaskState::Done(stored_done()),
        TaskState::Blocked(stored_done()),
        TaskState::Failed("x".to_owned()),
        TaskState::Cancelled,
        TaskState::Lost,
        TaskState::Skipped("why".to_owned()),
    ];
    let refs: Vec<&TaskState> = states.iter().collect();
    assert_eq!(counts_line(refs), "1 done, 2 failed, 1 cancelled, 1 lost");
}

fn stored_done() -> crate::orchestration::agents_tool::Report {
    let cell = ReportCell::default();
    submit(&cell, ReportStatus::Done, "ok");
    cell.get().expect("report stored")
}

#[test]
fn delivery_cancel_summary_has_no_previews() {
    let task = done_task("report body", &["src/a.rs"]);
    let tasks = [TaskNotice {
        id: task.id,
        label: "solo",
        state: &task.state,
        changed: &task.changed,
        preview_text: "report body",
        suffix: "",
    }];
    let sections = [StepNotice {
        name: "solo",
        tasks: tasks.to_vec(),
        pool: false,
        skipped: None,
    }];
    let text = cancel_summary(JobId::new_v7(), "run", "48.0s", &sections, RUN_NOTICE_LIMIT);
    assert!(text.contains("cancelled after 48.0s"));
    assert!(!text.contains("report body"));
    assert_eq!(text.matches(super::CLAIM_HONESTY).count(), 1);
}

#[test]
fn delivery_task_text_carries_every_path() {
    let id = JobId::new_v7();
    let run = JobId::new_v7();
    let text = task_text(
        id,
        "audit",
        run,
        "done",
        "48.0s",
        &["a".into(), "b".into(), "c".into(), "d".into()],
        "full report",
    );
    assert!(text.starts_with(&format!("task {id} \"audit\" of run {run}: done in 48.0s")));
    assert!(text.contains("changed: a, b, c, d"));
    assert!(text.ends_with("full report"));
}
