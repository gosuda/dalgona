// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The records of an eval run: one line per run, one row per case and arm,
//! and the printed report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

/// The report and run-line format version.
pub const FORMAT_VERSION: u32 = 1;

/// Which configuration a run used.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArmName {
    /// The product defaults with the user's `dal.toml`.
    Defaults,
    /// The defaults with the `--against` file layered on top.
    Against,
}

/// How one run ended. A skipped run is never a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The check or the judge accepted the run.
    Passed,
    /// The run failed, timed out, or could not be scored.
    Failed,
    /// The run was not scored: the judge gate was off, or the run was interrupted.
    Skipped,
}

/// How the judge gate resolved for a judge-scored run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeGate {
    /// The gate resolved on and the judge answered or failed.
    On,
    /// The gate is configured off.
    Off,
    /// The gate is `auto` and no usable judge resolved.
    Unresolved,
}

/// The judge's part in one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct JudgeLine {
    /// How the gate resolved.
    pub gate: JudgeGate,
    /// The bool verdict, when the judge answered.
    pub verdict: Option<bool>,
}

/// One line of `runs.jsonl`: the record of one run of one case under one arm.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunLine {
    /// The format version.
    pub v: u32,
    /// The pack name.
    pub pack: String,
    /// The case id.
    pub case: String,
    /// The arm that ran.
    pub arm: ArmName,
    /// The 1-based repeat number.
    pub repeat: u32,
    /// How the run ended.
    pub status: Status,
    /// Why a run failed or was skipped.
    pub reason: Option<String>,
    /// Input tokens, when the session ran.
    pub tokens_in: Option<u64>,
    /// Output tokens, when the session ran.
    pub tokens_out: Option<u64>,
    /// Wall time of the session in milliseconds, when it ran.
    pub latency_ms: Option<u64>,
    /// The provider-reported cost in USD, when reported.
    pub cost_usd: Option<f64>,
    /// The judge's part, for a judge-scored case.
    pub judge: Option<JudgeLine>,
}

impl RunLine {
    /// The one-line JSON of this record, as written to `runs.jsonl`.
    ///
    /// # Errors
    /// Returns the serializer error.
    pub fn to_json(&self) -> Result<String, sonic_rs::Error> {
        sonic_rs::to_string(self)
    }
}

/// The judge's part across the runs of one case and arm.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JudgeRow {
    /// How the gate resolved.
    pub gate: JudgeGate,
    /// The verdicts the judge gave, in run order.
    pub verdicts: Vec<bool>,
}

/// One case under one arm. The `arm` key appears only with `--against`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaseRow {
    /// The case id.
    pub id: String,
    /// The arm, present only in a comparison report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arm: Option<ArmName>,
    /// How many runs the arm planned.
    pub repeats: u32,
    /// Runs that passed.
    pub passed: u32,
    /// Runs that failed.
    pub failed: u32,
    /// Runs that were skipped.
    pub skipped: u32,
    /// `passed / repeats`, rounded to three decimals.
    pub pass_rate: f64,
    /// Mean input tokens over the runs whose session ran.
    pub tokens_in_mean: Option<f64>,
    /// Mean output tokens over the runs whose session ran.
    pub tokens_out_mean: Option<f64>,
    /// Mean session wall time in milliseconds.
    pub latency_ms_mean: Option<f64>,
    /// Mean provider-reported cost in USD; `null` when no run reported one.
    pub cost_usd_mean: Option<f64>,
    /// The judge's part, for a judge-scored case.
    pub judge: Option<JudgeRow>,
}

/// The `report.json` document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    /// The format version.
    pub v: u32,
    /// The pack name.
    pub pack: String,
    /// The model id every arm ran, as configured.
    pub model: String,
    /// The `--against` config path, or `null`.
    pub against: Option<String>,
    /// When the run started, RFC 3339 UTC with milliseconds.
    pub started_at: String,
    /// The case rows, in pack order; with `--against`, each case has one row per arm.
    pub cases: Vec<CaseRow>,
    /// Present and `true` when an interrupt cut the run short.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
}

/// What the aggregator needs to know about one planned case.
#[derive(Clone, Debug)]
pub struct CasePlan {
    /// The case id.
    pub id: String,
    /// Whether a judge scores the case.
    pub judged: bool,
}

/// The inputs of [`Report::build`].
#[derive(Debug)]
pub struct ReportInput<'a> {
    /// The pack name.
    pub pack: &'a str,
    /// The model id.
    pub model: &'a str,
    /// The `--against` path.
    pub against: Option<&'a str>,
    /// The start time, already formatted.
    pub started_at: &'a str,
    /// The planned cases in pack order.
    pub plan: &'a [CasePlan],
    /// Every run line, in the order the runs happened.
    pub lines: &'a [RunLine],
    /// Whether an interrupt cut the run short.
    pub interrupted: bool,
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

fn mean(values: impl Iterator<Item = f64>, places: i32) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0u32), |(sum, count), value| (sum + value, count + 1));
    (count > 0).then(|| round(sum / f64::from(count), places))
}

fn count(lines: &[&RunLine], status: Status) -> u32 {
    let matching = lines.iter().filter(|line| line.status == status).count();
    u32::try_from(matching).unwrap_or(u32::MAX)
}

fn judge_row(lines: &[&RunLine]) -> Option<JudgeRow> {
    let gate = lines
        .iter()
        .filter_map(|line| line.judge)
        .map(|judge| judge.gate)
        .find(|gate| *gate == JudgeGate::On)
        .or_else(|| {
            lines
                .iter()
                .find_map(|line| line.judge)
                .map(|judge| judge.gate)
        })?;
    let verdicts = lines
        .iter()
        .filter_map(|line| line.judge.and_then(|judge| judge.verdict))
        .collect();
    Some(JudgeRow { gate, verdicts })
}

fn row(id: &str, arm: Option<ArmName>, judged: bool, lines: &[&RunLine]) -> CaseRow {
    let repeats = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let passed = count(lines, Status::Passed);
    let pass_rate = if repeats == 0 {
        0.0
    } else {
        round(f64::from(passed) / f64::from(repeats), 3)
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "token counts and latencies are far below 2^53"
    )]
    let as_f64 = |value: u64| value as f64;
    CaseRow {
        id: id.to_owned(),
        arm,
        repeats,
        passed,
        failed: count(lines, Status::Failed),
        skipped: count(lines, Status::Skipped),
        pass_rate,
        tokens_in_mean: mean(lines.iter().filter_map(|l| l.tokens_in).map(as_f64), 3),
        tokens_out_mean: mean(lines.iter().filter_map(|l| l.tokens_out).map(as_f64), 3),
        latency_ms_mean: mean(lines.iter().filter_map(|l| l.latency_ms).map(as_f64), 3),
        cost_usd_mean: mean(lines.iter().filter_map(|l| l.cost_usd), 6),
        judge: if judged { judge_row(lines) } else { None },
    }
}

impl Report {
    /// Groups the run lines into one row per case, and per arm with `--against`.
    #[must_use]
    pub fn build(input: &ReportInput<'_>) -> Self {
        let mut grouped: BTreeMap<(&str, ArmName), Vec<&RunLine>> = BTreeMap::new();
        for line in input.lines {
            grouped
                .entry((line.case.as_str(), line.arm))
                .or_default()
                .push(line);
        }
        let comparing = input.against.is_some();
        let mut cases = Vec::new();
        for planned in input.plan {
            for arm in [ArmName::Defaults, ArmName::Against] {
                let Some(lines) = grouped.get(&(planned.id.as_str(), arm)) else {
                    continue;
                };
                cases.push(row(
                    &planned.id,
                    comparing.then_some(arm),
                    planned.judged,
                    lines,
                ));
            }
        }
        Self {
            v: FORMAT_VERSION,
            pack: input.pack.to_owned(),
            model: input.model.to_owned(),
            against: input.against.map(str::to_owned),
            started_at: input.started_at.to_owned(),
            cases,
            interrupted: input.interrupted,
        }
    }

    /// The `report.json` text: one compact JSON object.
    ///
    /// # Errors
    /// Returns the serializer error.
    pub fn to_json(&self) -> Result<String, sonic_rs::Error> {
        sonic_rs::to_string(self)
    }

    /// The printed report.
    #[must_use]
    pub fn render(&self) -> String {
        let repeats = self
            .cases
            .iter()
            .map(|case| case.repeats)
            .max()
            .unwrap_or(0);
        let mut text = format!(
            "Eval report: {} (model {}, repeats {repeats}, against {})\n",
            self.pack,
            self.model,
            self.against.as_deref().unwrap_or("none"),
        );
        if self.against.is_some() {
            self.render_comparison(&mut text);
        } else {
            for case in &self.cases {
                render_case(&mut text, case);
            }
        }
        let sum = |pick: fn(&CaseRow) -> u32| self.cases.iter().map(pick).sum::<u32>();
        let _ = writeln!(
            text,
            "totals: {} passed, {} failed, {} skipped",
            sum(|case| case.passed),
            sum(|case| case.failed),
            sum(|case| case.skipped),
        );
        if self.interrupted {
            text.push_str("interrupted: the remaining runs were skipped\n");
        }
        text
    }

    fn render_comparison(&self, text: &mut String) {
        let mut ids: Vec<&str> = Vec::new();
        for case in &self.cases {
            if !ids.contains(&case.id.as_str()) {
                ids.push(&case.id);
            }
        }
        for id in ids {
            let arm = |name| {
                self.cases
                    .iter()
                    .find(|case| case.id == id && case.arm == Some(name))
            };
            render_delta(text, id, arm(ArmName::Defaults), arm(ArmName::Against));
        }
    }
}

fn plain(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.0}"))
}

fn render_case(text: &mut String, case: &CaseRow) {
    let cost = case
        .cost_usd_mean
        .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.4}"));
    let _ = writeln!(
        text,
        "  {}  pass {}/{}  tokens {}/{}  {} ms  ${cost}",
        case.id,
        case.passed,
        case.repeats,
        plain(case.tokens_in_mean),
        plain(case.tokens_out_mean),
        plain(case.latency_ms_mean),
    );
}

fn percent(case: &CaseRow) -> f64 {
    case.pass_rate * 100.0
}

fn tokens(case: &CaseRow) -> Option<f64> {
    Some(case.tokens_in_mean? + case.tokens_out_mean?)
}

fn signed(base: Option<f64>, candidate: Option<f64>, places: usize) -> String {
    match (base, candidate) {
        (Some(base), Some(candidate)) => format!("{:+.places$}", candidate - base),
        _ => "n/a".to_owned(),
    }
}

fn render_delta(text: &mut String, id: &str, base: Option<&CaseRow>, candidate: Option<&CaseRow>) {
    let (Some(base), Some(candidate)) = (base, candidate) else {
        let _ = writeln!(text, "  {id}  pass n/a  tokens n/a  latency n/a  cost n/a");
        return;
    };
    let _ = writeln!(
        text,
        "  {id}  pass {:.1}% -> {:.1}% ({} pp)  tokens {}  latency {} ms  cost {}",
        percent(base),
        percent(candidate),
        signed(Some(percent(base)), Some(percent(candidate)), 1),
        signed(tokens(base), tokens(candidate), 0),
        signed(base.latency_ms_mean, candidate.latency_ms_mean, 0),
        signed(base.cost_usd_mean, candidate.cost_usd_mean, 4),
    );
}

#[cfg(test)]
mod tests;
