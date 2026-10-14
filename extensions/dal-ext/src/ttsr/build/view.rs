//! Precedence and buckets: duplicate resolution and agent views.
//!
//! [`view_for_agent`] walks the retained snapshot in load order (user files,
//! then each plugin's records and files, then project files) and sorts every
//! winner into the stream, always-apply, or rulebook bucket. The budget caps
//! admit in that load order; the finished buckets are re-sorted by name so
//! prompt and report order stay deterministic.

use std::sync::Arc;

use super::super::scope;
use super::super::value::{Origin, Problem, ProblemKind, Rule, Severity, cut_utf8};
use super::snapshot::{BuildState, Candidate, rulebook_line};
use super::{
    ALWAYS_BODY_LIMIT, ALWAYS_BUDGET_FULL_NOTE, ALWAYS_BUDGET_FULL_REASON, ALWAYS_TOTAL_LIMIT,
    BODY_OVER_LIMIT_NOTE, BODY_OVER_LIMIT_REASON, RULEBOOK_LINE_LIMIT, RULEBOOK_LISTING_LIMIT,
    RuleSet, SKIPPED, STREAM_CONDITION_LIMIT,
};

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

#[derive(Clone, Copy)]
enum JudgedScope {
    Include,
    Exclude,
}

pub(super) fn view_for_agent(state: Arc<BuildState>, agent: &str) -> RuleSet {
    view_impl(state, agent, JudgedScope::Include)
}

pub(super) fn view_for_agent_without_judged(state: Arc<BuildState>, agent: &str) -> RuleSet {
    view_impl(state, agent, JudgedScope::Exclude)
}

fn view_impl(state: Arc<BuildState>, agent: &str, judged: JudgedScope) -> RuleSet {
    let mut admission = Admission {
        state: &state,
        problems: state.base_problems.clone(),
        stream: Vec::new(),
        always: Vec::new(),
        rulebook: Vec::new(),
        stream_conditions: 0,
        always_bytes: 0,
        listing_bytes: 0,
    };
    // Candidates retain load order: user files, plugin records and files,
    // then project files. Budget caps fill in exactly that order.
    let selected: Vec<&Candidate> = state
        .candidates
        .iter()
        .filter(|candidate| {
            matches!(judged, JudgedScope::Include) || candidate.rule.judge.is_none()
        })
        .filter(|candidate| {
            candidate
                .rule
                .agents
                .as_ref()
                .is_none_or(|agents| agents.is_match(agent))
        })
        .collect();
    for candidate in selected {
        admission.admit(candidate);
    }
    let Admission {
        problems,
        mut stream,
        mut always,
        mut rulebook,
        ..
    } = admission;
    sort_bucket_by_name(&mut stream);
    sort_bucket_by_name(&mut always);
    sort_bucket_by_name(&mut rulebook);
    RuleSet {
        stream,
        always,
        rulebook,
        problems,
        agent: agent.into(),
        state,
    }
}

fn sort_bucket_by_name(bucket: &mut [Arc<Rule>]) {
    bucket.sort_by(|left, right| left.name.cmp(&right.name));
}

/// Whether an always-apply rule lands in the always bucket, falls through to
/// the rulebook, or is dropped for lacking both budget room and a description.
enum AlwaysBucket {
    Admitted,
    Rulebook,
    Dropped,
}

/// The per-view bucket walk with its budget counters.
struct Admission<'a> {
    state: &'a BuildState,
    problems: Vec<Problem>,
    stream: Vec<Arc<Rule>>,
    always: Vec<Arc<Rule>>,
    rulebook: Vec<Arc<Rule>>,
    stream_conditions: usize,
    always_bytes: usize,
    listing_bytes: usize,
}

impl Admission<'_> {
    fn admit(&mut self, candidate: &Candidate) {
        if self.admit_stream(candidate) {
            return;
        }
        match self.admit_always(candidate) {
            AlwaysBucket::Admitted | AlwaysBucket::Dropped => return,
            AlwaysBucket::Rulebook => {}
        }
        self.admit_rulebook(candidate);
    }

    fn admit_stream(&mut self, candidate: &Candidate) -> bool {
        let rule = candidate.rule.as_ref();
        if rule.conditions.is_empty() {
            return false;
        }
        if !self.state.watch {
            self.problems.push(watch_problem(rule));
            return false;
        }
        if !scope::reaches_output(&rule.scope) {
            self.problems.push(scope_problem(rule));
            return false;
        }
        self.problems
            .extend(candidate.compile_skips.iter().cloned());
        if candidate.compiled.is_empty() {
            return false;
        }
        if candidate.compiled.len() > STREAM_CONDITION_LIMIT - self.stream_conditions {
            self.problems.push(Problem {
                origin: rule.origin.clone(),
                kind: ProblemKind::Set,
                reason: "the stream rules already hold 256 conditions".to_owned(),
                consequence: "dalgon does not watch this rule.".to_owned(),
                severity: Severity::Skipped,
            });
            return false;
        }
        self.stream_conditions += candidate.compiled.len();
        self.problems.extend(candidate.empty_notes.iter().cloned());
        if rule.always_apply {
            self.note(
                rule,
                "\"alwaysApply: true\" has no effect because the rule has a condition",
            );
        }
        if rule.globs.is_some() {
            self.note(
                rule,
                "globs limit this rule to tool calls on matching paths, so it never fires on text or thinking",
            );
        }
        self.stream.push(Arc::clone(&candidate.rule));
        true
    }

    fn admit_always(&mut self, candidate: &Candidate) -> AlwaysBucket {
        let rule = candidate.rule.as_ref();
        if !rule.always_apply {
            return AlwaysBucket::Rulebook;
        }
        if rule.body.len() > ALWAYS_BODY_LIMIT {
            if rule.description.is_some() {
                self.note(rule, BODY_OVER_LIMIT_NOTE);
                return AlwaysBucket::Rulebook;
            }
            self.dropped(rule, BODY_OVER_LIMIT_REASON);
            return AlwaysBucket::Dropped;
        }
        let separator_bytes = usize::from(!self.always.is_empty()) * 2;
        let requested = rule.body.len().saturating_add(separator_bytes);
        if self.always_bytes.saturating_add(requested) > ALWAYS_TOTAL_LIMIT {
            if rule.description.is_some() {
                self.note(rule, ALWAYS_BUDGET_FULL_NOTE);
                return AlwaysBucket::Rulebook;
            }
            self.dropped(rule, ALWAYS_BUDGET_FULL_REASON);
            return AlwaysBucket::Dropped;
        }
        self.always_bytes += requested;
        self.always.push(Arc::clone(&candidate.rule));
        AlwaysBucket::Admitted
    }

    fn admit_rulebook(&mut self, candidate: &Candidate) {
        let rule = candidate.rule.as_ref();
        if rule.description.is_none() {
            self.problems.push(no_usable_condition_problem(rule));
            return;
        }
        let line = rulebook_line(self.state, rule);
        if line.len() > RULEBOOK_LINE_LIMIT {
            self.note(
                rule,
                format!(
                    "the rulebook line is longer than {RULEBOOK_LINE_LIMIT} bytes. dalgona cut the line at {RULEBOOK_LINE_LIMIT} bytes."
                ),
            );
        }
        let stored_len = cut_utf8(&line, RULEBOOK_LINE_LIMIT).len();
        let separator_bytes = usize::from(!self.rulebook.is_empty());
        let requested = stored_len.saturating_add(separator_bytes);
        if self.listing_bytes.saturating_add(requested) > RULEBOOK_LISTING_LIMIT {
            self.note(
                rule,
                format!(
                    "the rulebook listing of {RULEBOOK_LISTING_LIMIT} bytes is full. dalgona did not list this rule; read it with rule://{}.",
                    rule.name
                ),
            );
            return;
        }
        self.listing_bytes += requested;
        self.rulebook.push(Arc::clone(&candidate.rule));
    }

    /// One skipped rule past a budget with no description to list instead.
    fn dropped(&mut self, rule: &Rule, reason: &str) {
        self.problems.push(Problem {
            origin: rule.origin.clone(),
            kind: ProblemKind::Set,
            reason: reason.to_owned(),
            consequence: SKIPPED.to_owned(),
            severity: Severity::Skipped,
        });
    }

    fn note(&mut self, rule: &Rule, reason: impl Into<String>) {
        self.problems.push(set_note(rule, reason));
    }
}
