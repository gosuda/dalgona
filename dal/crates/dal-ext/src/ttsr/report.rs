//! The `dalgon rules` report and the offline `dalgon rules test` prover.
//!
//! [`run_rules`] renders the loaded set in named sections with the exact
//! line shapes below; [`run_test`] feeds one text through a fresh watch with
//! an interrupting budget and reports its fires. The command layer parses
//! flags and standard input and maps the report exit to the process exit.

use std::fmt::Write as _;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;

use dal_core::RulesConfig;

use super::build::{RuleBuildInput, RuleSet, set_for};
use super::gate::{Gate, resolve_cfg};
use super::matcher::Compiled;
use super::readers::EditStyle;
use super::value::{InterruptMode, RepeatMode, Rule, Severity, ToolPat, ToolScope};
use super::watch::{SourceKind, WatchBudget, create};

/// Longest offline test text in bytes; longer input is a usage error.
pub const TEST_TEXT_MAX_BYTES: usize = 1_048_576;

/// The rendered `dalgon rules` report and its process exit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RulesReport {
    /// The report lines, each terminated by `\n` unless empty.
    pub text: String,
    /// `1` when the problems hold a skipped entry, else `0`.
    pub exit: i32,
}

/// Which stream class the offline test feeds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TestSource {
    /// Assistant-visible text.
    Text,
    /// Reasoning text.
    Thinking,
    /// One tool item with the given path and the text as its added text.
    Tool,
}

/// The offline test input. `TEXT` equal to `-` is resolved to standard input
/// by the command layer before this call; over-long input fails here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestFlags {
    /// The stream class to feed.
    pub source: TestSource,
    /// The tool name for a tool source; defaults to `patch`.
    pub tool: String,
    /// The item path for a tool source.
    pub path: Option<String>,
    /// The text to feed.
    pub text: String,
}

/// The rendered offline test result and its process exit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestReport {
    /// One line per fire, or the no-fire line; each terminated by `\n`.
    pub text: String,
    /// `0` when a rule fired, `1` when none fired.
    pub exit: i32,
}

/// A `dalgon rules test` usage error; the display is the exact error line.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UsageError {
    /// `--tool` was given without `--source tool`.
    #[error("dalgon rules test: --tool needs --source tool.")]
    ToolNeedsToolSource,
    /// `--path` was given without `--source tool`.
    #[error("dalgon rules test: --path needs --source tool.")]
    PathNeedsToolSource,
    /// The text exceeds the input limit.
    #[error("dalgon rules test: the text is longer than 1048576 bytes.")]
    TextTooLong,
}

/// Renders the rules report for the loaded set.
///
/// `input` carries the registry snapshot (records, plugin files, tool names,
/// agent); `data_root` and `workspace` locate the file roots whose report
/// paths and no-rules hint name them.
#[must_use]
pub fn run_rules(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
) -> RulesReport {
    let set = set_for(input, data_root, workspace, cfg);
    if set.stream.is_empty()
        && set.always.is_empty()
        && set.rulebook.is_empty()
        && set.problems.is_empty()
    {
        return RulesReport {
            text: format!(
                "No rules. Add Markdown files to {} or {}.\n",
                data_root.join("rules").display(),
                workspace.join(".dal").join("rules").display()
            ),
            exit: 0,
        };
    }
    let mut text = String::new();
    render_stream(&mut text, &set, cfg);
    render_always(&mut text, &set);
    render_rulebook(&mut text, &set);
    let exit = render_problems(&mut text, &set);
    RulesReport { text, exit }
}

/// Proves one text against the loaded set without opening a session.
///
/// Builds a fresh gate and a watch with an interrupting budget over turn 1.
/// A tool source feeds one item with `flags.path` and `flags.text` as its
/// added text, never as JSON.
///
/// # Errors
///
/// Fails with [`UsageError`] when the flags are inconsistent or the text
/// exceeds [`TEST_TEXT_MAX_BYTES`].
pub fn run_test(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
    flags: &TestFlags,
) -> Result<TestReport, UsageError> {
    check_test_flags(flags)?;
    let set = set_for(input, data_root, workspace, cfg);
    let gate = Gate::default();
    let mut watch = create(
        &set,
        &gate,
        dal_core::TurnId::new(NonZeroU64::MIN),
        WatchBudget::Interrupts,
        cfg,
        workspace,
        EditStyle::Replace,
    );
    let _ = match flags.source {
        TestSource::Text => watch.feed(SourceKind::Text, &flags.text),
        TestSource::Thinking => watch.feed(SourceKind::Thinking, &flags.text),
        TestSource::Tool => watch.feed_added(&flags.tool, flags.path.as_deref(), &flags.text),
    };
    let _ = watch.finish();
    let mut text = String::new();
    for fire in watch.fires() {
        // A `String` never fails on write.
        let _ = writeln!(
            text,
            "fired: {} ({}). The {} matched /{}/.",
            fire.rule.as_str(),
            fire.action.as_str(),
            fire.subject.as_ref(),
            fire.pattern.as_ref()
        );
    }
    if watch.fires().is_empty() {
        // A `String` never fails on write.
        let _ = writeln!(
            text,
            "No rule fired. Checked {} stream rules.",
            set.stream.len()
        );
        return Ok(TestReport { text, exit: 1 });
    }
    Ok(TestReport { text, exit: 0 })
}

fn check_test_flags(flags: &TestFlags) -> Result<(), UsageError> {
    if flags.source != TestSource::Tool && flags.tool != "patch" {
        return Err(UsageError::ToolNeedsToolSource);
    }
    if flags.source != TestSource::Tool && flags.path.is_some() {
        return Err(UsageError::PathNeedsToolSource);
    }
    if flags.text.len() > TEST_TEXT_MAX_BYTES {
        return Err(UsageError::TextTooLong);
    }
    Ok(())
}

fn render_stream(text: &mut String, set: &RuleSet, cfg: &RulesConfig) {
    if set.stream.is_empty() {
        return;
    }
    // A `String` never fails on write.
    let _ = writeln!(text, "stream ({})", set.stream.len());
    for rule in &set.stream {
        let _ = writeln!(
            text,
            "  {}  {}",
            rule.name.as_str(),
            rule.origin.source_label()
        );
        let _ = writeln!(
            text,
            "    interrupt {}, {}{}",
            interrupt_literal(rule.interrupt_mode.unwrap_or(cfg.interrupt)),
            repeat_text(rule, cfg),
            extra_text(rule)
        );
        for condition in kept_conditions(set, rule) {
            let _ = writeln!(text, "    condition: {condition}");
        }
        let _ = writeln!(text, "    scope: {}", scope_tokens(rule).join(", "));
    }
}

fn render_always(text: &mut String, set: &RuleSet) {
    if set.always.is_empty() {
        return;
    }
    let _ = writeln!(text, "always-apply ({})", set.always.len());
    for rule in &set.always {
        let _ = writeln!(
            text,
            "  {}  {}",
            rule.name.as_str(),
            rule.origin.source_label()
        );
    }
}

fn render_rulebook(text: &mut String, set: &RuleSet) {
    if set.rulebook.is_empty() {
        return;
    }
    let _ = writeln!(text, "rulebook ({})", set.rulebook.len());
    for rule in &set.rulebook {
        let _ = writeln!(
            text,
            "  {}  {}",
            rule.name.as_str(),
            rule.origin.source_label()
        );
        let _ = writeln!(
            text,
            "    {}",
            rule.description.as_deref().unwrap_or_default()
        );
    }
}

/// Renders the problems section; returns the report exit.
fn render_problems(text: &mut String, set: &RuleSet) -> i32 {
    let mut exit = 0;
    if set.problems.is_empty() {
        return exit;
    }
    let _ = writeln!(text, "problems ({})", set.problems.len());
    for problem in &set.problems {
        let entry = problem_text(problem);
        if problem.severity == Severity::Note {
            let _ = writeln!(text, "  note: {}: {entry}", problem.origin.source_label());
        } else {
            exit = 1;
            let _ = writeln!(text, "  {}: {entry}", problem.origin.source_label());
        }
    }
    exit
}

fn interrupt_literal(mode: InterruptMode) -> &'static str {
    match mode {
        InterruptMode::Always => "always",
        InterruptMode::ProseOnly => "prose-only",
        InterruptMode::ToolOnly => "tool-only",
        // The core enum is non-exhaustive; an unknown future mode must not
        // interrupt.
        _ => "never",
    }
}

fn repeat_text(rule: &Rule, cfg: &RulesConfig) -> String {
    let resolved = resolve_cfg(rule, cfg);
    match resolved.mode {
        RepeatMode::AfterGap => format!("repeat every {} turns", resolved.gap),
        // The core enum is non-exhaustive; an unknown future mode repeats
        // like `once`.
        _ => "repeat once".to_owned(),
    }
}

fn extra_text(rule: &Rule) -> String {
    let mut extra = String::new();
    if rule.report {
        extra.push_str(", report only");
    }
    if rule.judge.is_some() {
        extra.push_str(", judged");
    }
    extra
}

fn kept_conditions(set: &RuleSet, rule: &Rule) -> Vec<String> {
    let kept: Option<Vec<String>> =
        set.compiled_conditions(rule.name.as_str())
            .map(|conditions: &[Arc<Compiled>]| {
                conditions
                    .iter()
                    .map(|condition| condition.src().to_owned())
                    .collect()
            });
    kept.unwrap_or_else(|| {
        rule.conditions
            .iter()
            .map(|condition| condition.src.to_string())
            .collect()
    })
}

fn scope_tokens(rule: &Rule) -> Vec<String> {
    let mut tokens = Vec::new();
    if rule.scope.text {
        tokens.push("text".to_owned());
    }
    if rule.scope.thinking {
        tokens.push("thinking".to_owned());
    }
    match &rule.scope.tools {
        ToolScope::All => tokens.push("tool".to_owned()),
        ToolScope::Tools(patterns) => {
            for pattern in patterns {
                tokens.push(tool_token(pattern));
            }
        }
    }
    tokens
}

fn tool_token(pattern: &ToolPat) -> String {
    let glob = pattern.glob.as_ref().map(globset::GlobMatcher::glob);
    match (pattern.tool.as_ref(), glob) {
        ("*", None) => "tool".to_owned(),
        ("*", Some(glob)) => format!("tool({glob})"),
        (tool, None) => format!("tool:{tool}"),
        (tool, Some(glob)) => format!("tool:{tool}({glob})"),
    }
}

fn problem_text(problem: &super::value::Problem) -> String {
    if problem.consequence.is_empty() {
        problem.reason.clone()
    } else if problem.severity == Severity::Note {
        // Plan 6533: key notes read `"<key>" is not supported. dal ignores
        // it.` Skipped entries keep the space join (plan 6671).
        format!("{}. {}", problem.reason, problem.consequence)
    } else {
        format!("{} {}", problem.reason, problem.consequence)
    }
}

#[cfg(test)]
mod tests;
