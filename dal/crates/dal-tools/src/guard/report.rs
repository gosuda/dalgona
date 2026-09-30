use super::metrics::Metric;
use std::fmt;

pub(super) const PLACEHOLDER_REJECT: &str = "PATCH REJECTED. The replacement contains an omission placeholder. Write the complete replacement text; nothing changed.";
pub(super) const GUARD_WRAP_NOTICE: &str = "GUARD-WRAP NOTICE. This edit adds a guard or fallback around unchanged code without removing behavior. Confirm that the guard is required; do not add a wrapper merely to avoid the failure.";
pub(super) const HELPER_NOTICE: &str = "ABSTRACTION NOTICE. This new helper is small and has one same-file use. Keep it if it names a real boundary; otherwise inline it. No change was made.";
pub(super) const BASELINES_RESET: &str = "guard: baselines reset (session start)";
pub(super) const FS_READ_DENIED: &str =
    "guard: fs.read denied; warning router disabled this session";
pub(super) const TURN_DENIED: &str = "guard: turn service denied; strike stop-downgrade active";
pub(super) const HUMAN_BASELINE: &str = " (human baseline 0.34±0.22)";
pub(super) const MINIMALISM_RULE: &str = "Minimalism rule: make the smallest change that meets the request. Add no guard, fallback, helper, or comment that the request does not need.";
pub(super) const CONTRACTS_HEADER: &str = "Tool contracts (terse):";
pub(super) const CONTRACTS_FOOTER: &str = "Each contract follows the minimalism rule above.";

pub(super) fn parse_reject(path: &str, location: u32, cause: &str) -> String {
    format!(
        "EDIT REJECTED. The file is not syntactically valid after this change. No change persists. Cause: {cause}. Re-read {path}:{location}, then submit one different edit. Do not repeat this edit verbatim."
    )
}

pub(super) fn broad_handler_notice(path: &str, line: u32) -> String {
    format!(
        "BROAD-HANDLER NOTICE. A new catch-all handler was added at {path}:{line}. Verify that the exception or unmatched case is intentionally handled."
    )
}

pub(super) fn strike_notice(n: u8, effect: &str, cause: &str, evidence: &str) -> String {
    format!(
        "<guard strike=\"{n}/3\">This call failed: {effect}. Cause: {cause}. Evidence: {evidence}. Next: re-read that region and submit one different edit. Do not repeat this call verbatim.</guard>"
    )
}

pub(super) fn exhaustion(key: &str, cause: &str, paths: &str) -> String {
    format!(
        "Guard stopped this turn after 3 strikes. No pending retry was run. Repeated target: {key}. Last failure: {cause}. Persistent changes in this turn: {paths}."
    )
}

pub(super) fn ledger(added: u64, deleted: u64, files: usize, new_files: usize) -> String {
    let net = i128::from(added) - i128::from(deleted);
    format!(
        "TURN CHANGE SUMMARY. Added {added}, deleted {deleted}, net {net}, files {files}, new files {new_files}. This is a report only; no automatic rewrite was started."
    )
}

pub(super) fn deletion_share(share: f64) -> String {
    format!("deletion share {share:.2}")
}

pub(super) fn pure_addition(path: &str) -> String {
    format!("pure-addition {path}")
}

pub(super) fn new_warning(warning: &str, path: &str, line: u32) -> String {
    format!(
        "NEW WARNING NOTICE. {warning} appeared in {path}:{line} after this turn's edit. Inspect it before claiming completion."
    )
}

pub(super) fn stream_interrupt(rule: &str) -> String {
    format!(
        "<system-interrupt reason=\"guard_high_certainty\">TOOL CALL BLOCKED BEFORE EXECUTION. Rule: {rule}. Nothing changed. Submit corrected arguments once; do not add compensating code.</system-interrupt>"
    )
}

pub(super) fn stream_report(rule: &str, count: u32) -> String {
    format!("guard: {rule} matched {count}x (report-only)")
}

pub(super) fn metrics_line(
    files: usize,
    crossed: usize,
    before: f64,
    after: f64,
    rose: bool,
) -> String {
    let mut line =
        format!("METRICS: files {files}, bands crossed {crossed}, erosion {before:.2}→{after:.2}");
    if rose {
        line.push_str(HUMAN_BASELINE);
    }
    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Before {
    New,
    Value(u32),
}

impl fmt::Display for Before {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::New => f.write_str("new"),
            Self::Value(value) => value.fmt(f),
        }
    }
}

pub(super) fn band_line(
    name: &str,
    metric: Metric,
    before: Before,
    after: u32,
    threshold: u32,
) -> String {
    format!("f {name} {metric} {before}→{after} (over {threshold})")
}

pub(super) fn file_band_line(path: &str, before: Before, after: u32, threshold: u32) -> String {
    format!("file {path} ploc {before}→{after} (over {threshold})")
}

pub(super) fn mass_line(name: &str, before: f64, after: f64) -> String {
    format!("f {name} mass {before:.1}→{after:.1}")
}

pub(super) fn churn_line(path: &str, count: u32) -> String {
    format!("churn {path} touched {count} times this turn")
}

pub(super) fn receipt(
    path: &str,
    ploc: u32,
    functions: usize,
    cog_sum: u32,
    cc_sum: u32,
) -> String {
    format!("guard: {path} ploc {ploc} functions {functions} cog-sum {cog_sum} cc-sum {cc_sum}")
}

pub(super) fn large_file(path: &str) -> String {
    format!("guard: {path} skipped (large file)")
}

pub(super) fn more_files(n: usize) -> String {
    format!("guard: +{n} more files")
}

pub(super) fn cannot_block(mechanism: &str) -> String {
    format!("guard: {mechanism} cannot block in v0")
}

pub(super) struct TurnSummary {
    pub added: u64,
    pub deleted: u64,
    pub files: Vec<Box<str>>,
    pub new_files: usize,
    pub reduction_ask: bool,
    pub pure_additions: Vec<Box<str>>,
    pub announce_reset: bool,
    pub bands: Vec<(f64, String)>,
    pub mass_rows: Vec<(f64, String)>,
    pub erosion: Option<(f64, f64)>,
    pub erosion_threshold: f64,
    pub churn: Vec<(Box<str>, u32)>,
    pub warnings: Vec<String>,
    pub stream: Vec<String>,
}

pub(super) fn turn_report(summary: &TurnSummary) -> Option<String> {
    let mut lines = Vec::new();
    if !summary.files.is_empty() {
        lines.push(ledger(
            summary.added,
            summary.deleted,
            summary.files.len(),
            summary.new_files,
        ));
        if summary.reduction_ask {
            let total = summary.added + summary.deleted;
            if total > 0 {
                lines.push(deletion_share(summary.deleted as f64 / total as f64));
            }
            lines.extend(
                summary
                    .pure_additions
                    .iter()
                    .map(|path| pure_addition(path)),
            );
        }
    }

    if summary.announce_reset {
        lines.push(BASELINES_RESET.to_owned());
    }

    let (before, after) = summary.erosion.unwrap_or((0.0, 0.0));
    let rose = after - before >= summary.erosion_threshold;
    if !summary.bands.is_empty() || rose || !summary.churn.is_empty() {
        lines.push(metrics_line(
            summary.files.len(),
            summary.bands.len(),
            before,
            after,
            rose,
        ));
        let mut bands: Vec<_> = summary.bands.iter().collect();
        bands.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        lines.extend(bands.into_iter().take(10).map(|(_, line)| line.clone()));
        if rose {
            let remaining = 10usize.saturating_sub(summary.bands.len().min(10));
            let mut mass_rows: Vec<_> = summary.mass_rows.iter().collect();
            mass_rows.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            lines.extend(
                mass_rows
                    .into_iter()
                    .take(remaining)
                    .map(|(_, line)| line.clone()),
            );
        }
        let mut churn: Vec<_> = summary.churn.iter().collect();
        churn.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        lines.extend(
            churn
                .into_iter()
                .take(3)
                .map(|(path, count)| churn_line(path, *count)),
        );
    }

    lines.extend(summary.warnings.iter().cloned());
    lines.extend(summary.stream.iter().cloned());
    (!lines.is_empty()).then(|| lines.join("\n"))
}
