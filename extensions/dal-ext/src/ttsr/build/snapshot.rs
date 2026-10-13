//! The retained build snapshot: compiled candidates and rule-set views.
//!
//! [`BuildState`] compiles every post-precedence winner once; the
//! [`RuleSet`] projections read the retained snapshot without touching the
//! filesystem.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::matcher::{self, Compiled};
use super::super::value::{Name, Origin, Problem, ProblemKind, Rule, Severity, cut_utf8};
use super::{RULEBOOK_LINE_LIMIT, RuleSet};

/// Renders one rulebook listing line for a rule: `- name (globs): description`.
///
/// The view walk measures the same rendering to enforce the rulebook line
/// and listing budgets, so the listing and its accounting cannot drift.
pub(super) fn rulebook_line(state: &BuildState, rule: &Rule) -> String {
    let mut line = format!("- {}", rule.name.as_str());
    if let Some(globs) = state.rulebook_globs.get(&rule.name)
        && !globs.is_empty()
    {
        line.push_str(" (");
        line.push_str(&globs.join(", "));
        line.push(')');
    }
    line.push_str(": ");
    line.push_str(
        &rule
            .description
            .as_deref()
            .unwrap_or_default()
            .replace(['\r', '\n'], " "),
    );
    line
}

impl RuleSet {
    /// Returns the concatenated always-apply bodies, separated by one blank
    /// line. Rules are already in deterministic name order.
    #[must_use]
    pub fn always_text(&self) -> String {
        let mut text = String::new();
        for rule in &self.always {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&rule.body);
        }
        text
    }

    /// Returns one rulebook entry per line, preserving authored glob strings.
    /// A line break in a description is rendered as one space, and a line
    /// over the rulebook line budget is cut to it.
    #[must_use]
    pub fn rulebook_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.rulebook.len());
        for rule in &self.rulebook {
            let line = rulebook_line(&self.state, rule);
            lines.push(cut_utf8(&line, RULEBOOK_LINE_LIMIT).to_owned());
        }
        lines.join("\n")
    }

    /// Returns the original bytes of a winning rule file, if this rule came
    /// from disk. Record rules have no file source and return `None`.
    #[must_use]
    pub fn source_bytes(&self, name: &str) -> Option<&[u8]> {
        self.state.source_bytes.get(name).map(Arc::<[u8]>::as_ref)
    }

    /// Returns the body of a winning rule by name: the text that `rule://`
    /// serves for a rule with no file source, such as a bundled pack rule.
    #[must_use]
    pub fn body_of(&self, name: &str) -> Option<&str> {
        let index = *self.state.candidate_by_name.get(name)?;
        Some(self.state.candidates[index].rule.body.as_ref())
    }

    /// Returns the winning rules of this agent view whose origin is a user
    /// rule file, in name order. A user file replaces a bundled rule of the
    /// same name before the stream watch sees either.
    #[must_use]
    pub fn user_rule_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .stream
            .iter()
            .chain(&self.always)
            .chain(&self.rulebook)
            .filter(|rule| matches!(rule.origin, Origin::User(_)))
            .map(|rule| rule.name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    /// Returns the precompiled matchers for a winning rule name.
    ///
    /// Stream consumers reuse these matchers rather than compiling a regular
    /// expression for each response. A valid rule name with no usable matcher
    /// returns an empty slice; an unknown name returns `None`.
    #[must_use]
    pub fn compiled_conditions(&self, name: &str) -> Option<&[Arc<Compiled>]> {
        let index = *self.state.candidate_by_name.get(name)?;
        Some(self.state.candidates[index].compiled.as_slice())
    }
}

impl RuleSet {
    /// Counts judged candidates retained for this agent view.
    #[must_use]
    pub fn judged_count(&self) -> usize {
        self.state
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.rule.judge.is_some()
                    && candidate
                        .rule
                        .agents
                        .as_ref()
                        .is_none_or(|agents| agents.is_match(self.agent.as_ref()))
            })
            .count()
    }

    /// Counts all skipped diagnostics from discovery and bucket selection.
    #[must_use]
    pub fn skipped_count(&self) -> usize {
        self.problems
            .iter()
            .filter(|problem| problem.severity == Severity::Skipped)
            .count()
    }
}

#[derive(Debug)]
pub(super) struct BuildState {
    pub(super) candidates: Vec<Candidate>,
    pub(super) candidate_by_name: HashMap<Name, usize>,
    pub(super) base_problems: Vec<Problem>,
    pub(super) watch: bool,
    pub(super) source_bytes: HashMap<Name, Arc<[u8]>>,
    pub(super) rulebook_globs: HashMap<Name, Vec<String>>,
}

impl BuildState {
    pub(super) fn new(
        winners: Vec<RuleCandidate>,
        base_problems: Vec<Problem>,
        watch: bool,
    ) -> Self {
        let mut candidates = Vec::with_capacity(winners.len());
        let mut candidate_by_name = HashMap::with_capacity(winners.len());
        let mut source_bytes = HashMap::new();
        let mut rulebook_globs = HashMap::new();

        for winner in winners {
            let RuleCandidate {
                rule,
                rulebook_globs: globs,
                shorthand_fallback,
                source_bytes: bytes,
            } = winner;
            let name = rule.name.clone();
            if let Some(bytes) = bytes {
                source_bytes.insert(name.clone(), bytes);
            }
            rulebook_globs.insert(name.clone(), globs);
            candidate_by_name.insert(name, candidates.len());
            candidates.push(Candidate::compile(rule, shorthand_fallback, watch));
        }

        Self {
            candidates,
            candidate_by_name,
            base_problems,
            watch,
            source_bytes,
            rulebook_globs,
        }
    }
}

#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) rule: Arc<Rule>,
    pub(super) compiled: Vec<Arc<Compiled>>,
    pub(super) compile_skips: Vec<Problem>,
    pub(super) empty_notes: Vec<Problem>,
}

impl Candidate {
    fn compile(rule: Rule, shorthand_fallback: bool, watch: bool) -> Self {
        let mut compiled = Vec::new();
        let mut compile_skips = Vec::new();
        let mut empty_notes = Vec::new();
        if watch && rule.scope.reaches_any() {
            for condition in &rule.conditions {
                match matcher::compile(&condition.src, condition.index) {
                    Ok(condition_matcher) => {
                        if condition_matcher.matches_empty() && !shorthand_fallback {
                            empty_notes.push(Problem {
                                origin: rule.origin.clone(),
                                kind: ProblemKind::SetNote,
                                reason: format!(
                                    "condition {} \"{}\" matches empty text, so it fires on the first output of every stream it watches.",
                                    condition.index + 1,
                                    condition.src
                                ),
                                consequence: String::new(),
                                severity: Severity::Note,
                            });
                        }
                        compiled.push(Arc::new(condition_matcher));
                    }
                    Err(skip) => compile_skips.push(skip.problem(rule.origin.clone())),
                }
            }
        }

        Self {
            rule: Arc::new(rule),
            compiled,
            compile_skips,
            empty_notes,
        }
    }
}

#[derive(Debug)]
pub(super) struct RuleCandidate {
    pub(super) rule: Rule,
    pub(super) rulebook_globs: Vec<String>,
    pub(super) shorthand_fallback: bool,
    pub(super) source_bytes: Option<Arc<[u8]>>,
}
