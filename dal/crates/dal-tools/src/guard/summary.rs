use std::collections::HashSet;

use super::{FunctionMetrics, GuardConfig, TurnState, metrics, report};

#[expect(
    clippy::too_many_lines,
    reason = "turn-summary assembly is one ordered report; extracting sections hides the emit order"
)]
pub(super) fn build(
    cfg: &GuardConfig,
    reset_due: &mut bool,
    announced_blocks: &mut bool,
    seen_warnings: &mut HashSet<(Box<str>, Box<str>)>,
    turn: &mut TurnState,
) -> Option<String> {
    let pure_additions: Vec<Box<str>> = turn
        .files
        .iter()
        .filter(|path| turn.deletions.get(*path).copied().unwrap_or_default() == 0)
        .cloned()
        .collect();
    let mut mass_rows = Vec::new();
    for (path, functions) in &turn.last_post {
        for function in functions {
            let before = turn
                .first_pre
                .get(path)
                .and_then(|before| metrics::match_in(before, function));
            let before_mass = before.map_or(0.0, metrics::mass);
            mass_rows.push((
                metrics::mass(function) - before_mass,
                report::mass_line(&function.name, before_mass, metrics::mass(function)),
            ));
        }
    }
    let erosion = (!turn.last_post.is_empty()).then(|| {
        let before: Vec<FunctionMetrics> = turn.first_pre.values().flatten().cloned().collect();
        let after: Vec<FunctionMetrics> = turn.last_post.values().flatten().cloned().collect();
        (
            metrics::erosion(before.iter()),
            metrics::erosion(after.iter()),
        )
    });
    let churn: Vec<(Box<str>, u32)> = turn
        .churn
        .iter()
        .filter(|(_, count)| **count >= cfg.churn_threshold)
        .map(|(path, count)| (path.clone(), *count))
        .collect();
    let mut stream_lines = Vec::new();
    for (rule, count) in &turn.stream_counts {
        if !cfg.calibrated.contains(rule) && *count > 0 {
            stream_lines.push(report::stream_report(rule.name(), *count));
        }
    }
    let mut warnings = Vec::new();
    let mut seen_in_turn = HashSet::new();
    for warning in &turn.warnings {
        if !turn.files.contains(&warning.path) {
            continue;
        }
        if !seen_warnings.insert(warning.key()) {
            continue;
        }
        if !seen_in_turn.insert(warning.path.clone()) {
            continue;
        }
        warnings.push(report::new_warning(
            &warning.text,
            &warning.path,
            warning.line,
        ));
    }
    let mut bands = turn.bands.clone();
    bands.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let summary = report::TurnSummary {
        added: turn.added,
        deleted: turn.deleted,
        files: turn.files.iter().cloned().collect(),
        new_files: turn.new_files.len(),
        reduction_ask: turn.reduction_ask,
        pure_additions,
        announce_reset: *reset_due,
        bands,
        mass_rows,
        erosion,
        erosion_threshold: cfg.erosion_threshold,
        churn,
        warnings,
        stream: stream_lines,
    };
    *reset_due = false;
    let mut lines = Vec::new();
    if !cfg.cannot_block.is_empty() && !*announced_blocks {
        *announced_blocks = true;
        lines.extend(
            cfg.cannot_block
                .iter()
                .map(|mechanism| report::cannot_block(mechanism)),
        );
    }
    let pending = std::mem::take(&mut turn.pending_notices);
    let report = report::turn_report(&summary);
    match (lines.is_empty() && pending.is_empty(), report) {
        (true, report) => report,
        (false, None) => Some(
            lines
                .into_iter()
                .chain(pending)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (false, Some(report)) => {
            lines.push(report);
            lines.extend(pending);
            Some(lines.join("\n"))
        }
    }
}
