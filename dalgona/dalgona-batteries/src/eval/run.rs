// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The eval runner: ephemeral cases over an [`Executor`], with a hard case
//! timeout, interrupt handling, and judge gating.
//!
//! The runner owns the case workspace, the clock, and the scoring. An
//! [`Executor`] owns one arm's configuration and runs one prompt in one
//! ephemeral session; the dalgona binary supplies the host-backed executor.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::pack::{Case, CheckInput, Exit, Pack, Scoring, ToolCall, Usage};
use super::report::{
    ArmName, CasePlan, FORMAT_VERSION, JudgeGate, JudgeLine, Report, ReportInput, RunLine, Status,
};

/// The wall-clock limit of one run of one case.
pub const CASE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a cancelled executor may wind its session down before the runner
/// stops waiting for it.
const GRACE: Duration = Duration::from_secs(5);

/// The skip reason of a run that an interrupt cut short.
pub const INTERRUPTED: &str = "skipped: interrupted";

/// A boxed executor future.
pub type ExecFuture<'a> = Pin<Box<dyn Future<Output = Result<Outcome, ExecError>> + Send + 'a>>;

/// What an executor needs to run one prompt.
#[derive(Clone, Copy, Debug)]
pub struct RunRequest<'a> {
    /// The prompt to run.
    pub prompt: &'a str,
    /// The case workspace, already seeded with the case files.
    pub workspace: &'a Path,
    /// The tool names the run may use; `None` means the product default set.
    pub tools: Option<&'a [String]>,
    /// The judge question, when a judge scores the case.
    pub judge: Option<&'a str>,
}

/// Why the judge did not score a run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JudgeSkip {
    /// The gate is configured off.
    Off,
    /// The gate is `auto` and no usable judge resolved.
    Unresolved,
}

/// What the judge said about a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JudgeResult {
    /// No judge question was asked.
    NotAsked,
    /// The judge answered a bool question.
    Verdict(bool),
    /// The judge call failed; the text is the judge's error.
    Error(Box<str>),
}

/// The record of one finished session.
#[derive(Clone, Debug)]
pub struct RunOutput {
    /// How the session ended.
    pub exit: Exit,
    /// The assistant text.
    pub text: String,
    /// Every tool call, in order.
    pub tool_calls: Vec<ToolCall>,
    /// The usage of the run.
    pub usage: Usage,
    /// Wall time from prompt to turn end, in milliseconds.
    pub latency_ms: u64,
    /// The judge's answer, when a question was asked.
    pub judge: JudgeResult,
}

/// What an executor reports for one run.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// The session ran.
    Ran(RunOutput),
    /// The judge gate did not resolve on, so the session did not run.
    JudgeSkipped(JudgeSkip),
}

/// Why an executor could not produce a run.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The arm could not start, for example because its config failed to load.
    #[error("{0}")]
    Arm(Box<str>),
    /// The session could not open or run.
    #[error("{0}")]
    Session(Box<str>),
    /// An interrupt or the timeout cancelled the run.
    #[error("interrupted")]
    Interrupted,
}

/// One arm's way of running a prompt.
pub trait Executor: Send + Sync {
    /// Runs one prompt in one ephemeral session and winds the session down.
    ///
    /// The executor must stop and return when `cancel` fires.
    fn run<'a>(&'a self, request: RunRequest<'a>, cancel: CancellationToken) -> ExecFuture<'a>;
}

/// One arm of a run.
pub struct Arm {
    /// The arm name.
    pub name: ArmName,
    /// The arm's executor, or why the arm could not start. A failed arm fails
    /// each of its runs; it does not fail the report.
    pub executor: Result<Arc<dyn Executor>, ExecError>,
}

/// What the user selected.
#[derive(Clone, Debug)]
pub struct Options {
    /// The pack name, as resolved.
    pub pack_name: String,
    /// The model id every arm runs.
    pub model: String,
    /// The `--against` config path.
    pub against: Option<String>,
    /// Run only this case.
    pub case: Option<String>,
    /// Override every case's repeat count.
    pub repeats: Option<u32>,
}

/// A run's setup, before any session runs.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PlanError {
    /// `--case` names no case of the pack.
    #[error("no case \"{0}\" in this pack")]
    NoSuchCase(String),
    /// `--repeats` is outside 1 to 10.
    #[error("--repeats must be an integer from 1 to {}", super::pack::MAX_REPEATS)]
    Repeats,
}

impl Options {
    /// Checks the selection against the pack.
    ///
    /// # Errors
    /// Returns [`PlanError`] for an unknown case or an out-of-range repeat count.
    pub fn check(&self, pack: &Pack) -> Result<(), PlanError> {
        if let Some(repeats) = self.repeats
            && !(1..=super::pack::MAX_REPEATS).contains(&repeats)
        {
            return Err(PlanError::Repeats);
        }
        match &self.case {
            Some(id) if pack.cases.iter().all(|case| case.id != *id) => {
                Err(PlanError::NoSuchCase(id.clone()))
            }
            _ => Ok(()),
        }
    }

    fn selects(&self, case: &Case) -> bool {
        self.case.as_ref().is_none_or(|id| *id == case.id)
    }
}

/// A case workspace: a fresh temporary directory seeded with the case files.
struct TempWorkspace(PathBuf);

impl TempWorkspace {
    async fn create(files: Vec<(String, String)>) -> io::Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("dalgona-eval-{}", uuid::Uuid::new_v4().simple()));
        let target = dir.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&target)?;
            for (path, text) in files {
                let file = target.join(path);
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(file, text)?;
            }
            target.canonicalize()
        })
        .await
        .map_err(io::Error::other)?
        .map(Self)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    async fn remove(self) {
        let dir = self.0;
        // A leftover temp directory is harmless, so a failed cleanup is dropped.
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
    }
}

/// The fields every run line of one run shares.
struct Slot<'a> {
    pack: &'a str,
    case: &'a Case,
    arm: ArmName,
    repeat: u32,
}

impl Slot<'_> {
    fn line(&self, status: Status, reason: Option<String>) -> RunLine {
        RunLine {
            v: FORMAT_VERSION,
            pack: self.pack.to_owned(),
            case: self.case.id.clone(),
            arm: self.arm,
            repeat: self.repeat,
            status,
            reason,
            tokens_in: None,
            tokens_out: None,
            latency_ms: None,
            cost_usd: None,
            judge: None,
        }
    }

    fn failed(&self, reason: String) -> RunLine {
        self.line(Status::Failed, Some(reason))
    }

    fn skipped(&self, reason: &str) -> RunLine {
        self.line(Status::Skipped, Some(reason.to_owned()))
    }
}

/// The ways a timed run can end without an outcome.
enum Ended {
    Executor(ExecError),
    TimedOut,
}

async fn timed(
    executor: &dyn Executor,
    request: RunRequest<'_>,
    cancel: &CancellationToken,
) -> Result<Outcome, Ended> {
    let run_cancel = cancel.child_token();
    let mut future = executor.run(request, run_cancel.clone());
    tokio::select! {
        outcome = &mut future => outcome.map_err(Ended::Executor),
        () = tokio::time::sleep(CASE_TIMEOUT) => {
            run_cancel.cancel();
            // The executor gets a short grace to wind its session down.
            let _ = tokio::time::timeout(GRACE, &mut future).await;
            Err(Ended::TimedOut)
        }
    }
}

fn judge_gate(skip: JudgeSkip) -> (JudgeGate, &'static str) {
    match skip {
        JudgeSkip::Off => (JudgeGate::Off, "skipped: judge gate is off"),
        JudgeSkip::Unresolved => (JudgeGate::Unresolved, "skipped: judge unresolved"),
    }
}

fn starlark_failure(pack: &str, case: &str, error: &impl std::fmt::Display) -> String {
    format!("{pack}: case \"{case}\": {error}.")
}

/// Scores a finished session with the case's check or its judge verdict.
async fn score(
    pack: &Arc<Pack>,
    index: usize,
    slot: &Slot<'_>,
    output: RunOutput,
    workspace: &Path,
) -> RunLine {
    let mut line = slot.line(Status::Passed, None);
    line.tokens_in = Some(output.usage.input_tokens);
    line.tokens_out = Some(output.usage.output_tokens);
    line.latency_ms = Some(output.latency_ms);
    line.cost_usd = output.usage.cost_usd;
    match &slot.case.scoring {
        Scoring::Judge(_) => {
            let verdict = match &output.judge {
                JudgeResult::Verdict(verdict) => Some(*verdict),
                JudgeResult::NotAsked | JudgeResult::Error(_) => None,
            };
            line.judge = Some(JudgeLine {
                gate: JudgeGate::On,
                verdict,
            });
            match output.judge {
                JudgeResult::Verdict(true) => {}
                JudgeResult::Verdict(false) => {
                    line.status = Status::Failed;
                    line.reason = Some("the judge answered no".to_owned());
                }
                JudgeResult::NotAsked => {
                    line.status = Status::Failed;
                    line.reason = Some(format!("case {}: the judge was not asked", slot.case.id));
                }
                JudgeResult::Error(text) => {
                    line.status = Status::Failed;
                    line.reason = Some(format!("case {}: {text}", slot.case.id));
                }
            }
        }
        Scoring::Check(_) => {
            let checked = run_check(pack, index, output, workspace).await;
            match checked {
                Ok(true) => {}
                Ok(false) => {
                    line.status = Status::Failed;
                    line.reason = Some("the check returned false".to_owned());
                }
                Err(error) => {
                    line.status = Status::Failed;
                    line.reason = Some(starlark_failure(slot.pack, &slot.case.id, &error));
                }
            }
        }
    }
    line
}

/// The failure of one check: its Starlark error, or the blocking task's.
#[derive(Debug, thiserror::Error)]
enum CheckFailure {
    #[error(transparent)]
    Script(#[from] super::pack::ScriptError),
    #[error("starlark error at line 1: the check task failed: {0}")]
    Task(String),
}

async fn run_check(
    pack: &Arc<Pack>,
    index: usize,
    output: RunOutput,
    workspace: &Path,
) -> Result<bool, CheckFailure> {
    let pack = Arc::clone(pack);
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let Scoring::Check(check) = &pack.cases[index].scoring else {
            return Ok(false);
        };
        let input = CheckInput {
            exit: output.exit,
            text: &output.text,
            tool_calls: &output.tool_calls,
            usage: output.usage,
        };
        check.run(&input, &workspace).map_err(CheckFailure::Script)
    })
    .await
    .map_err(|error| CheckFailure::Task(error.to_string()))?
}

/// Runs one case once under one arm. The flag is `true` when an interrupt
/// ended the run.
async fn run_once(
    pack: &Arc<Pack>,
    index: usize,
    slot: &Slot<'_>,
    arm: &Arm,
    cancel: &CancellationToken,
) -> (RunLine, bool) {
    let executor = match &arm.executor {
        Ok(executor) => executor,
        Err(error) => return (slot.failed(error.to_string()), false),
    };
    let case = slot.case;
    let workspace = match TempWorkspace::create(case.files.clone()).await {
        Ok(workspace) => workspace,
        Err(error) => {
            let reason = format!("case {}: setup failed: {error}", case.id);
            return (slot.failed(reason), false);
        }
    };
    let judge = match &case.scoring {
        Scoring::Judge(question) => Some(question.as_str()),
        Scoring::Check(_) => None,
    };
    let request = RunRequest {
        prompt: &case.prompt,
        workspace: workspace.path(),
        tools: case.tools.as_deref(),
        judge,
    };
    let result = match timed(executor.as_ref(), request, cancel).await {
        Ok(Outcome::Ran(output)) => (
            score(pack, index, slot, output, workspace.path()).await,
            false,
        ),
        Ok(Outcome::JudgeSkipped(skip)) => {
            let (gate, reason) = judge_gate(skip);
            let mut line = slot.skipped(reason);
            line.judge = Some(JudgeLine {
                gate,
                verdict: None,
            });
            (line, false)
        }
        Err(Ended::TimedOut) => (
            slot.failed(format!(
                "case {} timed out after {} s",
                case.id,
                CASE_TIMEOUT.as_secs()
            )),
            false,
        ),
        Err(Ended::Executor(ExecError::Interrupted)) => (slot.skipped(INTERRUPTED), true),
        Err(Ended::Executor(error)) => (slot.failed(error.to_string()), false),
    };
    workspace.remove().await;
    result
}

/// Runs the pack and returns its report.
///
/// Cases run in pack order; within a case the arms run in order, and repeats
/// run innermost. `on_run` sees every run line as soon as its run ends. An
/// interrupt skips the current and every later run and marks the report.
///
/// # Errors
/// Returns the first error `on_run` reports; the run stops there.
pub async fn run(
    pack: &Arc<Pack>,
    options: &Options,
    arms: &[Arm],
    cancel: &CancellationToken,
    started_at: &str,
    on_run: &mut dyn FnMut(&RunLine) -> io::Result<()>,
) -> io::Result<Report> {
    let mut lines = Vec::new();
    let mut plan = Vec::new();
    let mut interrupted = false;
    for (index, case) in pack.cases.iter().enumerate() {
        if !options.selects(case) {
            continue;
        }
        plan.push(CasePlan {
            id: case.id.clone(),
            judged: matches!(case.scoring, Scoring::Judge(_)),
        });
        let repeats = options.repeats.unwrap_or(case.repeats);
        for arm in arms {
            for repeat in 1..=repeats {
                let slot = Slot {
                    pack: &options.pack_name,
                    case,
                    arm: arm.name,
                    repeat,
                };
                interrupted |= cancel.is_cancelled();
                let line = if interrupted {
                    slot.skipped(INTERRUPTED)
                } else {
                    let (line, cut) = run_once(pack, index, &slot, arm, cancel).await;
                    interrupted = cut;
                    line
                };
                on_run(&line)?;
                lines.push(line);
            }
        }
    }
    Ok(Report::build(&ReportInput {
        pack: &options.pack_name,
        model: &options.model,
        against: options.against.as_deref(),
        started_at,
        plan: &plan,
        lines: &lines,
        interrupted,
    }))
}

#[cfg(test)]
mod tests;
