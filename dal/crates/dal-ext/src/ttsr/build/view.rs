//! Precedence and buckets: duplicate resolution and agent views.
//!
//! [`view_for_agent`] walks the retained snapshot in name order and sorts
//! every winner into the stream, always-apply, or rulebook bucket.

use std::sync::Arc;

use super::super::scope;
use super::super::value::{Origin, Problem, ProblemKind, Rule, Severity};
use super::snapshot::{BuildState, Candidate};
use super::{ALWAYS_BODY_LIMIT, ALWAYS_TOTAL_LIMIT, RuleSet, SKIPPED, STREAM_CONDITION_LIMIT};

pub(super) fn origin_preference(left: &Origin, right: &Origin) -> std::cmp::Ordering {
    let rank = left.rank().cmp(&right.rank());
    if !rank.is_eq() {
        return rank;
    }

    match (plugin_name(left), plugin_name(right)) {
        (Some(left_plugin), Some(right_plugin)) => {
            let plugin_order = left_plugin.cmp(right_plugin);
            if !plugin_order.is_eq() {
                return plugin_order;
            }
            match (left, right) {
                (Origin::Record { .. }, Origin::Plugin { .. }) => std::cmp::Ordering::Less,
                (Origin::Plugin { .. }, Origin::Record { .. }) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        }
        _ => std::cmp::Ordering::Equal,
    }
}

fn plugin_name(origin: &Origin) -> Option<&str> {
    match origin {
        Origin::Plugin { plugin, .. } | Origin::Record { plugin } => Some(plugin),
        Origin::User(_) | Origin::Project(_) => None,
    }
}

pub(super) fn duplicate_problem(loser: &Rule, winner: &Rule) -> Problem {
    let plugin_replaced_by_user = matches!(&winner.origin, Origin::User(_))
        && matches!(&loser.origin, Origin::Plugin { .. } | Origin::Record { .. });
    let (kind, reason, consequence, severity) = if plugin_replaced_by_user {
        let plugin = plugin_name(&loser.origin).unwrap_or_default();
        (
            ProblemKind::SetNote,
            format!(
                "rule \"{}\" from plugin {plugin} is replaced by {}",
                loser.name,
                winner.origin.source_label()
            ),
            String::new(),
            Severity::Note,
        )
    } else {
        (
            ProblemKind::Set,
            format!(
                "rule \"{}\" is also defined by {}, which takes precedence",
                loser.name,
                winner.origin.source_label()
            ),
            "dalgon skipped this one.".to_owned(),
            Severity::Skipped,
        )
    };
    Problem {
        origin: loser.origin.clone(),
        kind,
        reason,
        consequence,
        severity,
    }
}
fn scope_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::Set,
        reason: "the scope reaches no output".to_owned(),
        consequence: "dalgon does not watch this rule.".to_owned(),
        severity: Severity::Skipped,
    }
}

fn watch_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::SetNote,
        reason: "rules.watch is false, so dalgon does not watch this rule".to_owned(),
        consequence: String::new(),
        severity: Severity::Note,
    }
}

fn no_usable_condition_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::Set,
        reason: "the rule has no usable condition, no \"alwaysApply: true\", and no description"
            .to_owned(),
        consequence: SKIPPED.to_owned(),
        severity: Severity::Skipped,
    }
}

fn set_note(rule: &Rule, reason: impl Into<String>) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::SetNote,
        reason: reason.into(),
        consequence: String::new(),
        severity: Severity::Note,
    }
}

pub(super) fn view_for_agent(state: Arc<BuildState>, agent: &str) -> RuleSet {
    let mut problems = state.base_problems.clone();
    let mut selected: Vec<&Candidate> = state
        .candidates
        .iter()
        .filter(|candidate| {
            candidate
                .rule
                .agents
                .as_ref()
                .is_none_or(|agents| agents.is_match(agent))
        })
        .collect();
    selected.sort_by(|left, right| left.rule.name.cmp(&right.rule.name));

    let mut stream = Vec::new();
    let mut always = Vec::new();
    let mut rulebook = Vec::new();
    let mut stream_conditions: usize = 0;
    let mut always_bytes: usize = 0;

    for candidate in selected {
        let rule = candidate.rule.as_ref();
        let has_conditions = !rule.conditions.is_empty();
        let stream_candidate = if !has_conditions {
            false
        } else if !state.watch {
            problems.push(watch_problem(rule));
            false
        } else if !scope::reaches_output(&rule.scope) {
            problems.push(scope_problem(rule));
            false
        } else {
            problems.extend(candidate.compile_skips.iter().cloned());
            if candidate.compiled.is_empty() {
                false
            } else if candidate.compiled.len() > STREAM_CONDITION_LIMIT - stream_conditions {
                problems.push(Problem {
                    origin: rule.origin.clone(),
                    kind: ProblemKind::Set,
                    reason: "the stream rules already hold 256 conditions".to_owned(),
                    consequence: "dalgon does not watch this rule.".to_owned(),
                    severity: Severity::Skipped,
                });
                false
            } else {
                stream_conditions += candidate.compiled.len();
                problems.extend(candidate.empty_notes.iter().cloned());
                if rule.always_apply {
                    problems.push(set_note(
                        rule,
                        "\"alwaysApply: true\" has no effect because the rule has a condition",
                    ));
                }
                if rule.globs.is_some() {
                    problems.push(set_note(
                        rule,
                        "globs limit this rule to tool calls on matching paths, so it never fires on text or thinking",
                    ));
                }
                stream.push(Arc::clone(&candidate.rule));
                true
            }
        };
        if stream_candidate {
            continue;
        }

        if rule.always_apply {
            let separator_bytes = if always.is_empty() { 0 } else { 2 };
            let requested = rule.body.len().saturating_add(separator_bytes);
            if rule.body.len() > ALWAYS_BODY_LIMIT
                || always_bytes.saturating_add(requested) > ALWAYS_TOTAL_LIMIT
            {
                problems.push(set_note(
                    rule,
                    "the always-apply budget is exhausted; this rule is listed in the rulebook instead.",
                ));
            } else {
                always_bytes += requested;
                always.push(Arc::clone(&candidate.rule));
                continue;
            }
        }
        if rule.description.is_some() {
            rulebook.push(Arc::clone(&candidate.rule));
        } else {
            problems.push(no_usable_condition_problem(rule));
        }
    }

    RuleSet {
        stream,
        always,
        rulebook,
        problems,
        state,
    }
}
