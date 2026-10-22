// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio_util::sync::CancellationToken;

use super::{
    Arm, ExecError, ExecFuture, Executor, INTERRUPTED, JudgeResult, JudgeSkip, Options, Outcome,
    PlanError, RunOutput, RunRequest, run,
};
use crate::eval::pack::{Exit, Pack, Usage, load};
use crate::eval::report::{ArmName, JudgeGate, RunLine, Status};

use std::sync::Arc;

fn pack(source: &str) -> Arc<Pack> {
    Arc::new(load("t.eval.star", source).expect("pack loads"))
}

const TWO_CASES: &str = "def good(r, w):\n    return True\ndef bad(r, w):\n    fail(\"boom\")\nevalpack.pack(name = \"p\", cases = [\n    evalpack.case(id = \"a\", prompt = \"x\", check = bad, repeats = 1),\n    evalpack.case(id = \"b\", prompt = \"y\", check = good, repeats = 1),\n])\n";

const SMOKE: &str = r##"def has_shebang(result, workspace):
    return result["exit"] == "ok" and workspace.read("scripts/run.sh").startswith("#!/bin/sh")

evalpack.pack(name = "edit-anchor-smoke", cases = [
    evalpack.case(
        id = "shebang",
        prompt = "Add a shebang line to scripts/run.sh so it runs as a shell script.",
        files = {"scripts/run.sh": "set -e\necho ok\n"},
        check = has_shebang,
        repeats = 3,
    ),
])
"##;
fn options() -> Options {
    Options {
        pack_name: "p".to_owned(),
        model: "m".to_owned(),
        against: None,
        case: None,
        repeats: None,
    }
}

fn output() -> RunOutput {
    RunOutput {
        exit: Exit::Ok,
        text: "done".to_owned(),
        tool_calls: Vec::new(),
        usage: Usage {
            input_tokens: 100,
            output_tokens: 10,
            cost_usd: None,
        },
        latency_ms: 50,
        judge: JudgeResult::NotAsked,
    }
}

/// What a fake executor does on one call.
enum Behavior {
    Done(Outcome),
    Fail(ExecError),
    Hang,
    Stuck,
    /// Writes files into the workspace, then returns the outcome.
    Edit(Vec<(&'static str, &'static str)>, Outcome),
}

struct Fake<F> {
    behave: F,
    calls: AtomicUsize,
    wound_down: AtomicUsize,
    workspaces: Mutex<Vec<PathBuf>>,
}

impl<F: Fn(usize, &RunRequest<'_>) -> Behavior + Send + Sync + 'static> Fake<F> {
    fn arm(name: ArmName, behave: F) -> (Arm, Arc<Self>) {
        let fake = Arc::new(Self {
            behave,
            calls: AtomicUsize::new(0),
            wound_down: AtomicUsize::new(0),
            workspaces: Mutex::new(Vec::new()),
        });
        let executor: Arc<dyn Executor> = fake.clone();
        (
            Arm {
                name,
                executor: Ok(executor),
            },
            fake,
        )
    }
}

impl<F: Fn(usize, &RunRequest<'_>) -> Behavior + Send + Sync + 'static> Executor for Fake<F> {
    fn run<'a>(&'a self, request: RunRequest<'a>, cancel: CancellationToken) -> ExecFuture<'a> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.workspaces
            .lock()
            .unwrap()
            .push(request.workspace.path().to_path_buf());
        let behavior = (self.behave)(call, &request);
        Box::pin(async move {
            match behavior {
                Behavior::Done(outcome) => Ok(outcome),
                Behavior::Fail(error) => Err(error),
                Behavior::Hang => {
                    cancel.cancelled().await;
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    self.wound_down.fetch_add(1, Ordering::SeqCst);
                    Err(ExecError::Interrupted)
                }
                Behavior::Stuck => std::future::pending().await,
                Behavior::Edit(files, outcome) => {
                    for (path, text) in files {
                        let file = request.workspace.path().join(path);
                        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                        std::fs::write(file, text).unwrap();
                    }
                    Ok(outcome)
                }
            }
        })
    }
}

async fn run_with(
    pack: &Arc<Pack>,
    options: &Options,
    arms: &[Arm],
    cancel: &CancellationToken,
) -> (crate::eval::report::Report, Vec<RunLine>) {
    let mut seen = Vec::new();
    let report = run(
        pack,
        options,
        arms,
        cancel,
        "2026-09-25T10:15:30.123Z",
        &mut |line| {
            seen.push(line.clone());
            Ok(())
        },
    )
    .await
    .expect("run finishes");
    (report, seen)
}

/// A check that always outlives a short deadline: it runs long enough to reach
/// the evaluator's infrequent stop check, which then ends it.
const SLOW_CHECK: &str = "def slow(r, w):\n    total = 0\n    for i in range(2000000000):\n        total = total + i\n    return True\nevalpack.pack(name = \"p\", cases = [\n    evalpack.case(id = \"s\", prompt = \"x\", check = slow, repeats = 1),\n])\n";

#[tokio::test]
async fn a_check_that_outlives_the_run_budget_fails_its_case() {
    let slow_pack = pack(SLOW_CHECK);
    let slot = super::Slot {
        pack: "p",
        case: &slow_pack.cases[0],
        arm: ArmName::Defaults,
        repeat: 1,
    };
    let workspace = super::RunWorkspace::create(Vec::new())
        .await
        .expect("case workspace");
    // Already expired by the worker's first stop check: the Tokio clock only
    // advances between this capture and the worker thread, and run_check
    // converts it once into the wall clock the synchronous hook reads, so the
    // evaluator ends the check on its first infrequent check and every select
    // arm converges on Timeout.
    let deadline = tokio::time::Instant::now();
    let (line, interrupted) = super::score(
        &slow_pack,
        0,
        &slot,
        output(),
        &workspace,
        deadline,
        &CancellationToken::new(),
    )
    .await;
    assert!(!interrupted);
    assert_eq!(line.status, Status::Failed);
    assert!(
        line.reason
            .as_deref()
            .is_some_and(|reason| reason.contains("exceeded the run time budget")),
        "{line:?}",
    );
}

#[tokio::test]
async fn an_interrupt_during_the_check_skips_the_run_and_releases_the_workspace() {
    let slow_pack = pack(SLOW_CHECK);
    let slot = super::Slot {
        pack: "p",
        case: &slow_pack.cases[0],
        arm: ArmName::Defaults,
        repeat: 1,
    };
    let workspace = super::RunWorkspace::create(Vec::new())
        .await
        .expect("case workspace");
    let cancel = CancellationToken::new();
    cancel.cancel();
    // Only the cancel arm can be ready: the stop signal is already fired, so
    // the evaluator ends the check on its first stop check.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    let (line, interrupted) = super::score(
        &slow_pack,
        0,
        &slot,
        output(),
        &workspace,
        deadline,
        &cancel,
    )
    .await;
    assert!(interrupted);
    assert_eq!(line.status, Status::Skipped);
    assert_eq!(line.reason.as_deref(), Some(INTERRUPTED));
    // score consumed the check thread before returning, so this lease is the
    // last one: dropping it removes the directory.
    let dir = workspace.path().to_path_buf();
    drop(workspace);
    assert!(!dir.exists(), "the check lease released the workspace");
}

#[tokio::test]
async fn one_run_line_per_run_and_a_half_pass_rate_when_one_of_two_fails() {
    let smoke = pack(SMOKE);
    let (arm, fake) = Fake::arm(ArmName::Defaults, |call, _| {
        let outcome = Outcome::Ran(output());
        if call == 0 {
            Behavior::Edit(vec![("scripts/run.sh", "#!/bin/sh\nset -e\n")], outcome)
        } else {
            Behavior::Done(outcome)
        }
    });
    let opts = Options {
        repeats: Some(2),
        ..options()
    };
    let (report, lines) = run_with(&smoke, &opts, &[arm], &CancellationToken::new()).await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].status, Status::Passed);
    assert_eq!(lines[1].status, Status::Failed);
    assert_eq!(lines[1].reason.as_deref(), Some("the check returned false"));
    assert!((report.cases[0].pass_rate - 0.5).abs() < f64::EPSILON);
    assert_eq!(report.cases[0].repeats, 2);
    // Each run got its own workspace, seeded and then removed.
    let spaces = fake.workspaces.lock().unwrap().clone();
    assert_eq!(spaces.len(), 2);
    assert_ne!(spaces[0], spaces[1]);
    assert!(
        spaces.iter().all(|dir| !dir.exists()),
        "temp workspaces leak"
    );
}

#[tokio::test]
async fn the_workspace_is_seeded_with_the_case_files_before_the_run() {
    let smoke = pack(SMOKE);
    let (arm, _) = Fake::arm(ArmName::Defaults, |_, request| {
        let seeded = std::fs::read_to_string(request.workspace.path().join("scripts/run.sh"));
        assert_eq!(seeded.unwrap(), "set -e\necho ok\n");
        Behavior::Done(Outcome::Ran(output()))
    });
    let opts = Options {
        repeats: Some(1),
        ..options()
    };
    run_with(&smoke, &opts, &[arm], &CancellationToken::new()).await;
}

#[tokio::test]
async fn a_raising_check_fails_its_case_and_the_run_continues() {
    let (arm, _) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let (report, lines) = run_with(
        &pack(TWO_CASES),
        &options(),
        &[arm],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].status, Status::Failed);
    let reason = lines[0].reason.as_deref().unwrap();
    assert!(
        reason.starts_with("p: case \"a\": starlark error at line 4: "),
        "{reason}"
    );
    assert_eq!(lines[1].status, Status::Passed);
    assert_eq!(report.cases.len(), 2);
}

#[tokio::test]
async fn a_judge_case_records_verdicts_only_when_the_gate_resolves_on() {
    let source = "evalpack.pack(name = \"p\", cases = [evalpack.case(id = \"j\", prompt = \"x\", check = evalpack.judge(\"Is it fine?\"), repeats = 1)])\n";
    let judged = pack(source);
    let answer = |judge| {
        let mut out = output();
        out.judge = judge;
        Outcome::Ran(out)
    };
    let cases: Vec<(Outcome, Status, Option<&str>, JudgeGate)> = vec![
        (
            answer(JudgeResult::Verdict(true)),
            Status::Passed,
            None,
            JudgeGate::On,
        ),
        (
            answer(JudgeResult::Verdict(false)),
            Status::Failed,
            Some("the judge answered no"),
            JudgeGate::On,
        ),
        (
            answer(JudgeResult::Error("judge timed out after 5 ms".into())),
            Status::Failed,
            Some("case j: judge timed out after 5 ms"),
            JudgeGate::On,
        ),
        (
            Outcome::JudgeSkipped(JudgeSkip::Off),
            Status::Skipped,
            Some("skipped: judge gate is off"),
            JudgeGate::Off,
        ),
        (
            Outcome::JudgeSkipped(JudgeSkip::Unresolved),
            Status::Skipped,
            Some("skipped: judge unresolved"),
            JudgeGate::Unresolved,
        ),
    ];
    for (outcome, status, reason, gate) in cases {
        let slot = Mutex::new(Some(outcome));
        let (arm, _) = Fake::arm(ArmName::Defaults, move |_, request| {
            assert_eq!(request.judge, Some("Is it fine?"));
            Behavior::Done(slot.lock().unwrap().take().unwrap())
        });
        let (report, lines) =
            run_with(&judged, &options(), &[arm], &CancellationToken::new()).await;
        assert_eq!(lines[0].status, status, "{reason:?}");
        assert_eq!(lines[0].reason.as_deref(), reason);
        assert_eq!(lines[0].judge.unwrap().gate, gate);
        assert_eq!(report.cases[0].judge.as_ref().unwrap().gate, gate);
        // A skipped judge case is never a failure, whatever the gate.
        if status == Status::Skipped {
            assert_eq!(report.cases[0].failed, 0);
            assert_eq!(report.cases[0].tokens_in_mean, None);
        }
    }
}

#[tokio::test]
async fn a_failed_arm_fails_only_its_runs() {
    let smoke = pack(SMOKE);
    let (defaults, _) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Edit(
            vec![("scripts/run.sh", "#!/bin/sh\n")],
            Outcome::Ran(output()),
        )
    });
    let against = Arm {
        name: ArmName::Against,
        executor: Err(ExecError::Arm("dal.toml: unknown key \"edit_styl\"".into())),
    };
    let opts = Options {
        against: Some("cfg.toml".to_owned()),
        repeats: Some(1),
        ..options()
    };
    let (report, lines) = run_with(
        &smoke,
        &opts,
        &[defaults, against],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].arm, ArmName::Defaults);
    assert_eq!(lines[0].status, Status::Passed);
    assert_eq!(lines[1].arm, ArmName::Against);
    assert_eq!(lines[1].status, Status::Failed);
    assert_eq!(
        lines[1].reason.as_deref(),
        Some("dal.toml: unknown key \"edit_styl\"")
    );
    assert_eq!(report.cases.len(), 2);
    assert_eq!(report.cases[0].passed, 1);
    assert_eq!(report.cases[1].failed, 1);
}

#[tokio::test(start_paused = true)]
async fn a_hung_case_times_out_after_300_s_and_the_run_continues() {
    let (arm, fake) = Fake::arm(ArmName::Defaults, |call, _| {
        if call == 0 {
            Behavior::Hang
        } else {
            Behavior::Done(Outcome::Ran(output()))
        }
    });
    let started = tokio::time::Instant::now();
    let (report, lines) = run_with(
        &pack(TWO_CASES),
        &options(),
        &[arm],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(lines[0].status, Status::Failed);
    assert_eq!(
        lines[0].reason.as_deref(),
        Some("case a timed out after 300 s")
    );
    assert_eq!(lines[1].status, Status::Passed);
    assert!(started.elapsed() >= super::CASE_TIMEOUT);
    assert_eq!(
        fake.wound_down.load(Ordering::SeqCst),
        1,
        "the timed-out executor must finish its session wind-down"
    );
    assert!(!report.interrupted);
}

#[tokio::test(start_paused = true)]
async fn an_executor_that_ignores_cancellation_cannot_block_later_cases() {
    let (arm, fake) = Fake::arm(ArmName::Defaults, |call, _| {
        if call == 0 {
            Behavior::Stuck
        } else {
            Behavior::Done(Outcome::Ran(output()))
        }
    });
    let pack = pack(TWO_CASES);
    let options = options();
    let cancel = CancellationToken::new();
    let arms = [arm];
    let (report, lines) = tokio::time::timeout(
        super::CASE_TIMEOUT + super::CLEANUP_GRACE + std::time::Duration::from_secs(1),
        run_with(&pack, &options, &arms, &cancel),
    )
    .await
    .expect("a stuck executor must not defeat the deadline");
    assert_eq!(lines[0].status, Status::Failed);
    assert_eq!(lines[1].status, Status::Passed);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    assert!(!report.interrupted);
}

#[tokio::test]
async fn the_workspace_lives_until_its_last_owner_finishes() {
    let workspace = super::RunWorkspace::create(vec![("note".into(), "keep me".into())])
        .await
        .expect("create workspace");
    let path = workspace.path().to_path_buf();
    let cleanup_owner = workspace.clone();
    drop(workspace);
    assert_eq!(
        std::fs::read_to_string(path.join("note")).expect("executor still owns its workspace"),
        "keep me"
    );
    drop(cleanup_owner);
    assert!(!path.exists(), "the final owner removes the workspace");
}

#[tokio::test(start_paused = true)]
async fn slow_owned_cleanup_retains_the_workspace_past_the_timeout_report() {
    struct Slow {
        tasks: Mutex<tokio::task::JoinSet<()>>,
    }
    impl Executor for Slow {
        fn run<'a>(&'a self, request: RunRequest<'a>, cancel: CancellationToken) -> ExecFuture<'a> {
            let (done, finished) = tokio::sync::oneshot::channel();
            self.tasks.lock().unwrap().spawn(async move {
                cancel.cancelled().await;
                tokio::time::sleep(super::CLEANUP_GRACE * 2).await;
                assert!(
                    request.workspace.path().is_dir(),
                    "cleanup must retain the workspace after the runner moves on"
                );
                drop(request.workspace);
                let _ = done.send(());
            });
            Box::pin(async move {
                let _ = finished.await;
                Err(ExecError::Interrupted)
            })
        }
    }
    let slow = Slow {
        tasks: Mutex::new(tokio::task::JoinSet::new()),
    };
    let workspace = super::RunWorkspace::create(Vec::new()).await.unwrap();
    let path = workspace.path().to_path_buf();
    let result = super::timed(
        &slow,
        RunRequest {
            prompt: "p",
            workspace: workspace.clone(),
            tools: None,
            judge: None,
        },
        &CancellationToken::new(),
        tokio::time::Instant::now() + super::CASE_TIMEOUT,
    )
    .await;
    assert!(matches!(result, Err(super::Ended::TimedOut)));
    drop(workspace);
    assert!(path.is_dir(), "the executor still owns cleanup");
    let mut tasks = std::mem::take(&mut *slow.tasks.lock().unwrap());
    tasks.join_next().await.unwrap().unwrap();
    assert!(!path.exists(), "cleanup released the final workspace lease");
}

#[tokio::test]
async fn an_interrupt_after_the_first_case_skips_the_rest_and_marks_the_report() {
    let source = "def good(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = good, repeats = 1), evalpack.case(id = \"b\", prompt = \"x\", check = good, repeats = 2), evalpack.case(id = \"c\", prompt = \"x\", check = good, repeats = 1)])\n";
    let (arm, fake) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let cancel = CancellationToken::new();
    let mut seen: Vec<RunLine> = Vec::new();
    let report = run(
        &pack(source),
        &options(),
        &[arm],
        &cancel,
        "2026-09-25T10:15:30.123Z",
        &mut |line| {
            seen.push(line.clone());
            if seen.len() == 1 {
                cancel.cancel();
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        1,
        "no session starts after the interrupt"
    );
    assert_eq!(seen.len(), 4, "every planned run still gets one line");
    assert_eq!(seen[0].status, Status::Passed);
    assert!(seen[1..].iter().all(|line| line.status == Status::Skipped));
    assert!(
        seen[1..]
            .iter()
            .all(|line| line.reason.as_deref() == Some(INTERRUPTED))
    );
    assert!(report.interrupted);
    let totals: u32 = report
        .cases
        .iter()
        .map(|row| row.passed + row.failed + row.skipped)
        .sum();
    assert_eq!(usize::try_from(totals).unwrap(), seen.len());
    assert_eq!(report.cases.iter().map(|row| row.failed).sum::<u32>(), 0);
}

#[tokio::test]
async fn an_interrupt_during_a_run_skips_that_run_rather_than_failing_it() {
    let (arm, _) = Fake::arm(ArmName::Defaults, |call, _| {
        if call == 0 {
            Behavior::Fail(ExecError::Interrupted)
        } else {
            Behavior::Done(Outcome::Ran(output()))
        }
    });
    let (report, lines) = run_with(
        &pack(TWO_CASES),
        &options(),
        &[arm],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(lines[0].status, Status::Skipped);
    assert_eq!(lines[1].status, Status::Skipped);
    assert!(report.interrupted);
    assert_eq!(report.cases.iter().map(|row| row.failed).sum::<u32>(), 0);
}

#[tokio::test]
async fn an_executor_failure_fails_the_run_with_its_text() {
    let (arm, _) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Fail(ExecError::Session("cannot open the session".into()))
    });
    let (_, lines) = run_with(
        &pack(TWO_CASES),
        &options(),
        &[arm],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(lines[0].reason.as_deref(), Some("cannot open the session"));
    assert_eq!(lines[1].status, Status::Failed);
}

#[tokio::test]
async fn an_unwritable_seed_file_fails_with_a_setup_error_and_never_runs() {
    let source = "def good(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"s\", prompt = \"x\", check = good, repeats = 1, files = {\"a\": \"x\", \"a/b\": \"y\"})])\n";
    let (arm, fake) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let (_, lines) = run_with(&pack(source), &options(), &[arm], &CancellationToken::new()).await;
    assert_eq!(lines[0].status, Status::Failed);
    assert!(
        lines[0]
            .reason
            .as_deref()
            .unwrap()
            .starts_with("case s: setup failed: "),
        "{:?}",
        lines[0].reason
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn case_and_repeat_selection_narrow_the_run() {
    let (arm, fake) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let opts = Options {
        case: Some("b".to_owned()),
        repeats: Some(3),
        ..options()
    };
    let (report, lines) =
        run_with(&pack(TWO_CASES), &opts, &[arm], &CancellationToken::new()).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    assert_eq!(lines.len(), 3);
    assert_eq!(report.cases.len(), 1);
    assert_eq!(report.cases[0].id, "b");
}

#[test]
fn selection_errors_name_the_problem() {
    let loaded = pack(TWO_CASES);
    let unknown = Options {
        case: Some("zzz".to_owned()),
        ..options()
    };
    assert_eq!(
        unknown.check(&loaded),
        Err(PlanError::NoSuchCase("zzz".to_owned()))
    );
    for repeats in [0, 11] {
        let bad = Options {
            repeats: Some(repeats),
            ..options()
        };
        assert_eq!(bad.check(&loaded), Err(PlanError::Repeats));
    }
    assert_eq!(options().check(&loaded), Ok(()));
}

#[tokio::test]
async fn arms_run_in_order_with_repeats_innermost() {
    let source = "def good(r, w):\n    return True\nevalpack.pack(name = \"p\", cases = [evalpack.case(id = \"a\", prompt = \"x\", check = good, repeats = 2)])\n";
    let (defaults, _) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let (against, _) = Fake::arm(ArmName::Against, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let opts = Options {
        against: Some("cfg.toml".to_owned()),
        ..options()
    };
    let (_, lines) = run_with(
        &pack(source),
        &opts,
        &[defaults, against],
        &CancellationToken::new(),
    )
    .await;
    let order: Vec<(ArmName, u32)> = lines.iter().map(|line| (line.arm, line.repeat)).collect();
    assert_eq!(
        order,
        vec![
            (ArmName::Defaults, 1),
            (ArmName::Defaults, 2),
            (ArmName::Against, 1),
            (ArmName::Against, 2),
        ]
    );
}

#[tokio::test]
async fn a_failing_run_log_stops_the_run_with_its_io_error() {
    let (arm, fake) = Fake::arm(ArmName::Defaults, |_, _| {
        Behavior::Done(Outcome::Ran(output()))
    });
    let error = run(
        &pack(TWO_CASES),
        &options(),
        &[arm],
        &CancellationToken::new(),
        "t",
        &mut |_| Err(std::io::Error::other("disk full")),
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "disk full");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}
