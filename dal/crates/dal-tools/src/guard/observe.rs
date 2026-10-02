use std::collections::BTreeSet;

use super::{Engine, FileFindings, GuardConfig, Rule, TurnState, Verdict, checks, metrics, report};
use crate::patch::{DiffLineKind, EditFinding, FindingSeverity, StagedBatch};

struct Ctx<'a> {
    cfg: &'a GuardConfig,
    turn: &'a mut TurnState,
    displayed: &'a mut usize,
    out: &'a mut Vec<EditFinding>,
}

impl crate::patch::EditObserver for Engine {
    fn inspect(&self, batch: &StagedBatch<'_>) -> Vec<EditFinding> {
        if !self.cfg.enabled {
            return Vec::new();
        }
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let Some(session) = state.sessions.get_mut(&batch.session) else {
            return Vec::new();
        };
        let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == batch.turn) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut displayed = 0_usize;
        for file in &batch.files {
            let stop = {
                let mut ctx = Ctx {
                    cfg: &self.cfg,
                    turn,
                    displayed: &mut displayed,
                    out: &mut out,
                };
                inspect_file(&mut ctx, file)
            };
            if stop {
                return out;
            }
        }
        if displayed > 20 {
            out.push(EditFinding {
                rule: "metrics".into(),
                severity: FindingSeverity::Report,
                text: report::more_files(displayed - 20).into(),
            });
        }
        out
    }
}

fn inspect_file(ctx: &mut Ctx<'_>, file: &crate::patch::StagedFile<'_>) -> bool {
    let path: Box<str> = file.path.to_string_lossy().into();
    let Some(post) = file.after else {
        let removed = file.before.map_or(0, pre_line_count);
        ctx.turn.deleted = ctx.turn.deleted.saturating_add(removed);
        ctx.turn.files.insert(path.clone());
        ctx.turn.touch(path);
        return false;
    };
    let hunks = checks::hunks_from_diff(file.hunks);
    let added_text = hunks
        .iter()
        .flat_map(|hunk| hunk.added.iter().map(|(_, line)| line.as_ref()))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(checks::Rejection(reason)) = checks::placeholder(&added_text) {
        ctx.out.push(EditFinding {
            rule: "placeholder".into(),
            severity: FindingSeverity::Block,
            text: reason.into(),
        });
        return true;
    }
    match checks::parse_gate(
        &path,
        file.absolute_path,
        file.before,
        post,
        file.pre_parse.as_deref(),
        file.post_parse.as_deref(),
    ) {
        checks::GateOutcome::Reject(checks::Rejection(reason)) => {
            ctx.out.push(EditFinding {
                rule: "parse".into(),
                severity: FindingSeverity::Block,
                text: reason.into(),
            });
            true
        }
        checks::GateOutcome::Pass(parsed) | checks::GateOutcome::Exempt(parsed) => {
            record_file(ctx, &path, file, post, &hunks, parsed);
            false
        }
        checks::GateOutcome::Skipped => {
            record_unmeasured(ctx.turn, &path, file);
            false
        }
    }
}

fn pre_line_count(before: &[u8]) -> u64 {
    if before.is_empty() {
        return 0;
    }
    let newlines = before
        .split(|byte| *byte == b'\n')
        .count()
        .saturating_sub(1) as u64;
    if before.last() == Some(&b'\n') {
        newlines
    } else {
        newlines.saturating_add(1)
    }
}

pub(super) fn hunk_counts(hunks: &[crate::patch::DiffHunk]) -> (u64, u64) {
    let mut added = 0_u64;
    let mut deleted = 0_u64;
    for hunk in hunks {
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Added => added = added.saturating_add(1),
                DiffLineKind::Removed => deleted = deleted.saturating_add(1),
                DiffLineKind::Context => {}
            }
        }
    }
    (added, deleted)
}

#[expect(
    clippy::too_many_lines,
    reason = "one inspection walks every staged file in place"
)]
fn record_file(
    ctx: &mut Ctx<'_>,
    path: &str,
    file: &crate::patch::StagedFile<'_>,
    post: &[u8],
    hunks: &[checks::Hunk],
    parsed: Option<&crate::parse::Parsed>,
) {
    let (added, deleted) = hunk_counts(file.hunks);
    ctx.turn.added = ctx.turn.added.saturating_add(added);
    ctx.turn.deleted = ctx.turn.deleted.saturating_add(deleted);
    ctx.turn.files.insert(path.into());
    if file.before.is_none() {
        ctx.turn.new_files.insert(path.into());
    }
    ctx.turn
        .deletions
        .entry(path.into())
        .and_modify(|total| *total = total.saturating_add(deleted))
        .or_insert(deleted);
    ctx.turn.touch(path.into());
    let Some(parsed) = parsed else {
        ctx.turn.findings.insert(
            path.into(),
            FileFindings {
                path: path.into(),
                verdict: Verdict::Clean,
                items: Vec::new(),
                metrics: None,
            },
        );
        return;
    };
    if post.len() > 524_288 {
        if *ctx.displayed < 20 {
            ctx.out.push(EditFinding {
                rule: "metrics".into(),
                severity: FindingSeverity::Report,
                text: report::large_file(path).into(),
            });
        }
        *ctx.displayed = ctx.displayed.saturating_add(1);
        ctx.turn.findings.insert(
            path.into(),
            FileFindings {
                path: path.into(),
                verdict: Verdict::Skipped,
                items: Vec::new(),
                metrics: None,
            },
        );
        return;
    }
    let Ok(post_text) = std::str::from_utf8(post) else {
        ctx.turn.findings.insert(
            path.into(),
            FileFindings {
                path: path.into(),
                verdict: Verdict::Skipped,
                items: Vec::new(),
                metrics: None,
            },
        );
        return;
    };
    let language = parsed.lang;
    let pre_metrics = file.before.and_then(|before| {
        file.pre_parse.as_deref().and_then(|pre| {
            (pre.lang == language).then(|| metrics::measure(language, &pre.tree, before))
        })
    });
    let post_metrics = metrics::measure(language, &parsed.tree, post);
    if !ctx.turn.first_pre.contains_key(path) {
        ctx.turn.first_pre.insert(
            path.into(),
            pre_metrics
                .clone()
                .map_or_else(Vec::new, |metrics| metrics.functions),
        );
    }
    ctx.turn
        .last_post
        .insert(path.into(), post_metrics.functions.clone());
    let mut items = Vec::new();
    if let Some(finding) = checks::guard_wrap(hunks)
        && ctx.cfg.guard_wrap
        && !checks::bypassed(finding.rule, post_text, finding.line)
    {
        items.push(finding);
    }
    if let Some(finding) = checks::broad_handler(language, hunks, path)
        && ctx.cfg.broad_handler
        && !checks::bypassed(finding.rule, post_text, finding.line)
    {
        items.push(finding);
    }
    if ctx.cfg.helper {
        items.extend(
            checks::helper(
                language,
                path,
                pre_metrics.as_ref(),
                parsed,
                post,
                &post_metrics,
            )
            .into_iter()
            .filter(|finding| !checks::bypassed(finding.rule, post_text, finding.line)),
        );
    }
    let mut added_lines = BTreeSet::new();
    for hunk in hunks {
        for (line, _) in &hunk.added {
            added_lines.insert(*line);
        }
    }
    items.extend(
        checks::commented_out(language, parsed, post, &added_lines)
            .into_iter()
            .filter(|finding| !checks::bypassed(finding.rule, post_text, finding.line)),
    );
    let crossings = metrics::crossings(
        path,
        pre_metrics.as_ref(),
        &post_metrics,
        ctx.cfg.cognitive_band,
        ctx.cfg.cyclomatic_band,
        ctx.cfg.function_ploc_band,
        ctx.cfg.nesting_band,
        ctx.cfg.file_ploc_band,
    );
    if *ctx.displayed < 20 {
        ctx.out.push(EditFinding {
            rule: "metrics".into(),
            severity: FindingSeverity::Report,
            text: report::receipt(
                path,
                post_metrics.ploc,
                post_metrics.functions.len(),
                post_metrics.cog_sum,
                post_metrics.cc_sum,
            )
            .into(),
        });
        for crossing in &crossings {
            ctx.out.push(EditFinding {
                rule: "metrics".into(),
                severity: FindingSeverity::Report,
                text: crossing.line.clone().into(),
            });
        }
    }
    *ctx.displayed = ctx.displayed.saturating_add(1);
    ctx.turn.bands.extend(
        crossings
            .into_iter()
            .map(|crossing| (crossing.delta_mass, crossing.line)),
    );
    for finding in &items {
        if matches!(
            finding.rule,
            Rule::GuardWrap | Rule::BroadHandler | Rule::Helper
        ) && ctx.turn.notices.insert((finding.rule, path.into()))
        {
            ctx.out.push(EditFinding {
                rule: finding.rule.name().into(),
                severity: FindingSeverity::Report,
                text: finding.text.clone(),
            });
        }
    }
    ctx.turn.findings.insert(
        path.into(),
        FileFindings {
            path: path.into(),
            verdict: if items.is_empty() {
                Verdict::Clean
            } else {
                Verdict::Findings
            },
            items,
            metrics: Some(post_metrics),
        },
    );
}

fn record_unmeasured(turn: &mut TurnState, path: &str, file: &crate::patch::StagedFile<'_>) {
    let (added, deleted) = hunk_counts(file.hunks);
    turn.added = turn.added.saturating_add(added);
    turn.deleted = turn.deleted.saturating_add(deleted);
    turn.files.insert(path.into());
    if file.before.is_none() {
        turn.new_files.insert(path.into());
    }
    turn.touch(path.into());
    turn.findings.insert(
        path.into(),
        FileFindings {
            path: path.into(),
            verdict: Verdict::Skipped,
            items: Vec::new(),
            metrics: None,
        },
    );
}
