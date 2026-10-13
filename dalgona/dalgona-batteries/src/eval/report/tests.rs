// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use super::{ArmName, CasePlan, JudgeGate, JudgeLine, Report, ReportInput, RunLine, Status};

fn line(case: &str, arm: ArmName, repeat: u32, status: Status) -> RunLine {
    RunLine {
        v: 1,
        pack: "p".to_owned(),
        case: case.to_owned(),
        arm,
        repeat,
        status,
        reason: None,
        tokens_in: None,
        tokens_out: None,
        latency_ms: None,
        cost_usd: None,
        judge: None,
    }
}

fn ran(
    case: &str,
    arm: ArmName,
    repeat: u32,
    status: Status,
    tokens: (u64, u64),
    ms: u64,
) -> RunLine {
    RunLine {
        tokens_in: Some(tokens.0),
        tokens_out: Some(tokens.1),
        latency_ms: Some(ms),
        ..line(case, arm, repeat, status)
    }
}

fn plan(ids: &[&str]) -> Vec<CasePlan> {
    ids.iter()
        .map(|id| CasePlan {
            id: (*id).to_owned(),
            judged: false,
        })
        .collect()
}

fn build(against: Option<&str>, plan: &[CasePlan], lines: &[RunLine], interrupted: bool) -> Report {
    Report::build(&ReportInput {
        pack: "p",
        model: "m",
        against,
        started_at: "2026-09-25T10:15:30.123Z",
        plan,
        lines,
        interrupted,
    })
}

#[test]
fn the_report_json_matches_the_documented_shape_byte_for_byte() {
    let lines = [
        RunLine {
            cost_usd: Some(0.004),
            ..ran(
                "shebang",
                ArmName::Defaults,
                1,
                Status::Passed,
                (1200, 50),
                9000,
            )
        },
        RunLine {
            cost_usd: Some(0.0044),
            ..ran(
                "shebang",
                ArmName::Defaults,
                2,
                Status::Passed,
                (1260, 62),
                9300,
            )
        },
        ran(
            "shebang",
            ArmName::Defaults,
            3,
            Status::Failed,
            (1240, 56),
            9069,
        ),
    ];
    let report = build(None, &plan(&["shebang"]), &lines, false);
    assert_eq!(
        report.to_json().unwrap(),
        r#"{"v":1,"pack":"p","model":"m","against":null,"started_at":"2026-09-25T10:15:30.123Z","cases":[{"id":"shebang","repeats":3,"passed":2,"failed":1,"skipped":0,"pass_rate":0.667,"tokens_in_mean":1233.333,"tokens_out_mean":56.0,"latency_ms_mean":9123.0,"cost_usd_mean":0.0042,"judge":null}]}"#
    );
}

#[test]
fn a_missing_cost_is_null_and_a_skipped_run_never_counts_as_failed() {
    let lines = [
        ran("a", ArmName::Defaults, 1, Status::Passed, (10, 2), 100),
        RunLine {
            reason: Some("skipped: interrupted".to_owned()),
            ..line("a", ArmName::Defaults, 2, Status::Skipped)
        },
    ];
    let report = build(None, &plan(&["a"]), &lines, true);
    let row = &report.cases[0];
    assert_eq!(
        (row.passed, row.failed, row.skipped, row.repeats),
        (1, 0, 1, 2)
    );
    assert_eq!(row.pass_rate, 0.5);
    assert_eq!(row.cost_usd_mean, None);
    assert_eq!(row.tokens_in_mean, Some(10.0));
    assert!(report.to_json().unwrap().ends_with("\"interrupted\":true}"));
}

#[test]
fn totals_equal_the_sum_of_the_case_rows_and_the_line_count() {
    let lines = [
        ran("a", ArmName::Defaults, 1, Status::Passed, (1, 1), 1),
        ran("a", ArmName::Defaults, 2, Status::Failed, (1, 1), 1),
        line("b", ArmName::Defaults, 1, Status::Skipped),
    ];
    let report = build(None, &plan(&["a", "b"]), &lines, false);
    let passed: u32 = report.cases.iter().map(|row| row.passed).sum();
    let failed: u32 = report.cases.iter().map(|row| row.failed).sum();
    let skipped: u32 = report.cases.iter().map(|row| row.skipped).sum();
    assert_eq!(
        usize::try_from(passed + failed + skipped).unwrap(),
        lines.len()
    );
    assert!(
        report
            .render()
            .ends_with("totals: 1 passed, 1 failed, 1 skipped\n")
    );
}

#[test]
fn the_printed_report_has_the_documented_form() {
    let lines = [
        RunLine {
            cost_usd: Some(0.0042),
            ..ran(
                "shebang",
                ArmName::Defaults,
                1,
                Status::Passed,
                (1234, 56),
                9123,
            )
        },
        RunLine {
            cost_usd: Some(0.0042),
            ..ran(
                "shebang",
                ArmName::Defaults,
                2,
                Status::Failed,
                (1234, 56),
                9123,
            )
        },
    ];
    let report = build(None, &plan(&["shebang"]), &lines, false);
    assert_eq!(
        report.render(),
        "Eval report: p (model m, repeats 2, against none)\n  shebang  pass 1/2  tokens 1234/56  9123 ms  $0.0042\ntotals: 1 passed, 1 failed, 0 skipped\n"
    );
}

#[test]
fn a_case_with_no_data_prints_n_a() {
    let lines = [line("a", ArmName::Defaults, 1, Status::Failed)];
    let report = build(None, &plan(&["a"]), &lines, false);
    assert_eq!(
        report.render(),
        "Eval report: p (model m, repeats 1, against none)\n  a  pass 0/1  tokens n/a/n/a  n/a ms  $n/a\ntotals: 0 passed, 1 failed, 0 skipped\n"
    );
}

#[test]
fn a_comparison_prints_paired_deltas_candidate_minus_baseline() {
    let lines = [
        ran("c", ArmName::Defaults, 1, Status::Passed, (1000, 100), 5000),
        ran("c", ArmName::Defaults, 2, Status::Failed, (1000, 100), 5000),
        RunLine {
            cost_usd: Some(0.01),
            ..ran("c", ArmName::Against, 1, Status::Passed, (900, 100), 4000)
        },
        RunLine {
            cost_usd: Some(0.01),
            ..ran("c", ArmName::Against, 2, Status::Passed, (900, 100), 4000)
        },
    ];
    let report = build(Some("cfg.toml"), &plan(&["c"]), &lines, false);
    assert_eq!(
        report.render(),
        "Eval report: p (model m, repeats 2, against cfg.toml)\n  c  pass 50.0% -> 100.0% (+50.0 pp)  tokens -100  latency -1000 ms  cost n/a\ntotals: 3 passed, 1 failed, 0 skipped\n"
    );
    assert_eq!(report.cases.len(), 2);
    assert_eq!(report.cases[0].arm, Some(ArmName::Defaults));
    assert_eq!(report.cases[1].arm, Some(ArmName::Against));
    assert!(report.to_json().unwrap().contains("\"arm\":\"against\""));
}

#[test]
fn an_arm_without_data_prints_n_a_deltas() {
    let lines = [
        ran("c", ArmName::Defaults, 1, Status::Passed, (10, 10), 10),
        line("c", ArmName::Against, 1, Status::Failed),
    ];
    let report = build(Some("cfg.toml"), &plan(&["c"]), &lines, false);
    assert_eq!(
        report.render(),
        "Eval report: p (model m, repeats 1, against cfg.toml)\n  c  pass 100.0% -> 0.0% (-100.0 pp)  tokens n/a  latency n/a ms  cost n/a\ntotals: 1 passed, 1 failed, 0 skipped\n"
    );
}

#[test]
fn a_judge_case_reports_its_gate_and_verdicts() {
    let judged = |verdict: Option<bool>, gate: JudgeGate, status: Status| RunLine {
        judge: Some(JudgeLine { gate, verdict }),
        ..ran("j", ArmName::Defaults, 1, status, (1, 1), 1)
    };
    let planned = [CasePlan {
        id: "j".to_owned(),
        judged: true,
    }];
    let on = build(
        None,
        &planned,
        &[judged(Some(true), JudgeGate::On, Status::Passed)],
        false,
    );
    assert_eq!(on.cases[0].judge.as_ref().unwrap().verdicts, vec![true]);
    let off = build(
        None,
        &planned,
        &[judged(None, JudgeGate::Off, Status::Skipped)],
        false,
    );
    let row = off.cases[0].judge.as_ref().unwrap();
    assert_eq!(row.gate, JudgeGate::Off);
    assert_eq!(row.verdicts.len(), 0);
    assert_eq!(off.cases[0].failed, 0);
}
