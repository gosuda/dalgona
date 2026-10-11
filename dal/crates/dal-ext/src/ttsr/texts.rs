//! Byte-exact TTSR prompt text, status notes, and front-end projections.

use std::fmt::{self, Write as _};

use dal_core::TurnId;

use super::value::{Origin, Rule, RuleAction, cut_utf8};

/// Maximum number of UTF-8 bytes retained from a tool path in a subject.
pub const SUBJECT_PATH_MAX_BYTES: usize = 200;

/// Maximum number of UTF-8 bytes retained in a body-derived fire description.
pub const DESCRIPTION_FALLBACK_MAX_BYTES: usize = 120;

/// Which product command name appears in the rules-load status note.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RulesProduct {
    /// The `dalgon` product.
    Dalgon,
    /// The `dalgona` product.
    Dalgona,
}

impl RulesProduct {
    /// Returns the command name used by this product's rule-status text.
    #[must_use]
    pub const fn command_name(self) -> &'static str {
        match self {
            Self::Dalgon => "dalgon",
            Self::Dalgona => "dalgona",
        }
    }
}

/// The stream location that a rule matched, in its canonical displayed form.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuleSubject<'a> {
    /// Assistant response text.
    Reply,
    /// Assistant reasoning text.
    Reasoning,
    /// Tool-call arguments without a path-qualified item.
    ToolCall {
        /// The tool name.
        tool: &'a str,
    },
    /// Tool-call arguments associated with a path-qualified item.
    ToolCallOnPath {
        /// The tool name.
        tool: &'a str,
        /// The item's path, cut to [`SUBJECT_PATH_MAX_BYTES`] at a UTF-8 boundary.
        path: &'a str,
    },
}

impl RuleSubject<'_> {
    /// Renders the subject exactly as it appears in TTSR text and status notes.
    #[must_use]
    pub fn render(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for RuleSubject<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reply => formatter.write_str("reply"),
            Self::Reasoning => formatter.write_str("reasoning"),
            Self::ToolCall { tool } => {
                formatter.write_char('`')?;
                formatter.write_str(tool)?;
                formatter.write_str("` call")
            }
            Self::ToolCallOnPath { tool, path } => {
                formatter.write_char('`')?;
                formatter.write_str(tool)?;
                formatter.write_str("` call on `")?;
                formatter.write_str(cut_utf8(path, SUBJECT_PATH_MAX_BYTES))?;
                formatter.write_char('`')
            }
        }
    }
}

/// The typed, borrowed inputs for all projections of one rule fire.
///
/// Constructing this value does not format or copy any strings. Call its
/// rendering methods only when the fire is delivered.
#[derive(Clone, Copy, Debug)]
pub struct FireText<'a> {
    rule: &'a Rule,
    pattern: &'a str,
    subject: RuleSubject<'a>,
    action: RuleAction,
}

impl<'a> FireText<'a> {
    /// Creates a borrowed renderer for one fired rule.
    #[must_use]
    pub const fn new(
        rule: &'a Rule,
        pattern: &'a str,
        subject: RuleSubject<'a>,
        action: RuleAction,
    ) -> Self {
        Self {
            rule,
            pattern,
            subject,
            action,
        }
    }

    /// Returns the rule action associated with this fire.
    #[must_use]
    pub const fn action(self) -> RuleAction {
        self.action
    }

    /// Returns the exact model-facing interrupt or reminder text, or `None`
    /// for a report-only fire.
    #[must_use]
    pub fn model_text(self) -> Option<String> {
        match self.action {
            RuleAction::Interrupt => Some(render_interrupt_text(self.rule)),
            RuleAction::Remind => Some(render_reminder_text(self.rule, self.subject)),
            RuleAction::Report => None,
        }
    }

    /// Returns the TUI projection, or `None` for a report-only fire.
    #[must_use]
    pub fn tui_projection(self) -> Option<TuiProjection> {
        let follow_up = match self.action {
            RuleAction::Interrupt => TuiFollowUp::RetryWithRuleApplied,
            RuleAction::Remind => TuiFollowUp::ModelSeesRuleNextRequest,
            RuleAction::Report => return None,
        };
        Some(TuiProjection {
            copy: format!(
                "rule {} fired. The {} matched /{}/.",
                self.rule.name.as_str(),
                self.subject,
                self.pattern
            ),
            follow_up,
            remove_live_assistant: self.action == RuleAction::Interrupt,
        })
    }

    /// Returns the headless line for interrupt/remind fires, or `None` for a
    /// report-only fire.
    #[must_use]
    pub fn headless_line(self) -> Option<String> {
        match self.action {
            RuleAction::Interrupt | RuleAction::Remind => Some(format!(
                "dalgon: rule {} fired: {}",
                self.rule.name.as_str(),
                fire_description(self.rule)
            )),
            RuleAction::Report => None,
        }
    }

    /// Returns the RPC event value. Unlike user-facing projections, this also
    /// represents report-only fires so a program can count them.
    #[must_use]
    pub fn rpc_event(self, turn: TurnId) -> RpcRuleFired<'a> {
        RpcRuleFired {
            turn,
            name: self.rule.name.as_str(),
            description: fire_description(self.rule),
            pattern: self.pattern,
            subject: self.subject.render(),
            action: self.action.as_str(),
        }
    }

    /// Returns the shared notice body for ACP `_dal/notice`, router SSE
    /// comments, and A2A `dal.notice`, or `None` for a report-only fire.
    #[must_use]
    pub fn notice_text(self) -> Option<String> {
        match self.action {
            RuleAction::Interrupt | RuleAction::Remind => Some(format!(
                "rule {} fired: {}",
                self.rule.name.as_str(),
                fire_description(self.rule)
            )),
            RuleAction::Report => None,
        }
    }
}

/// The additional line shown in the TUI for an interrupt or reminder.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TuiFollowUp {
    /// The turn will retry with the rule applied.
    RetryWithRuleApplied,
    /// The model will see the rule with its next request.
    ModelSeesRuleNextRequest,
}

impl TuiFollowUp {
    /// Returns the exact follow-up text displayed by the TUI.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::RetryWithRuleApplied => "The turn retries with the rule applied.",
            Self::ModelSeesRuleNextRequest => "The model sees the rule with its next request.",
        }
    }
}

/// Text and the response-block disposition for one visible TUI rule fire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TuiProjection {
    /// The main copy line: `rule {name} fired. The {subject} matched /{pattern}/.`
    pub copy: String,
    /// The action-specific follow-up line.
    pub follow_up: TuiFollowUp,
    /// Whether the current live assistant block must be removed.
    pub remove_live_assistant: bool,
}

/// The typed RPC projection of one rule fire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcRuleFired<'a> {
    /// The turn in which the rule fired.
    pub turn: TurnId,
    /// The rule name.
    pub name: &'a str,
    /// The rule description or body-derived fallback.
    pub description: &'a str,
    /// The matched condition source.
    pub pattern: &'a str,
    /// The rendered subject.
    pub subject: String,
    /// The wire action literal: `interrupt`, `remind`, or `report`.
    pub action: &'static str,
}

/// Returns the rule description or its first body line cut at a UTF-8 boundary.
#[must_use]
pub fn fire_description(rule: &Rule) -> &str {
    rule.description.as_deref().unwrap_or_else(|| {
        cut_utf8(
            rule.body.lines().next().unwrap_or(""),
            DESCRIPTION_FALLBACK_MAX_BYTES,
        )
    })
}

/// Renders the interrupt prompt, without adding a trailing newline.
#[must_use]
pub fn render_interrupt_text(rule: &Rule) -> String {
    InterruptText(rule).to_string()
}

/// Renders the reminder prompt, without adding a trailing newline.
#[must_use]
pub fn render_reminder_text(rule: &Rule, subject: RuleSubject<'_>) -> String {
    ReminderText { rule, subject }.to_string()
}

/// Renders the judge-auto-off session-start notice.
#[must_use]
pub fn judge_auto_off_note(judged_rule_count: usize) -> String {
    format!(
        "rules: judge auto resolved to off: no judge model with usable credentials. {judged_rule_count} judged rules are not watched."
    )
}

/// Renders the skipped-rule load notice, omitting it when the count is zero.
#[must_use]
pub fn load_problems_note(product: RulesProduct, skipped_count: usize) -> Option<String> {
    if skipped_count == 0 {
        return None;
    }
    let command = product.command_name();
    let noun = if skipped_count == 1 { "part" } else { "parts" };
    Some(format!(
        "rules: {command} skipped {skipped_count} {noun} of your rules. Run \"{command} rules\" to see why."
    ))
}

/// Renders the once-per-turn retry-cap notice.
#[must_use]
pub fn retry_limit_note(rule_interrupts: usize) -> String {
    format!(
        "rules: this turn reached rules.max_retries ({rule_interrupts} rule interrupts). Later rule matches in this turn reach the model as reminders."
    )
}

/// Renders the info-level successful judge note.
#[must_use]
pub fn judge_pass_note(name: &str, subject: RuleSubject<'_>) -> String {
    format!("rules: judged rule \"{name}\" passed on {subject}")
}

/// Renders the info-level fail-open judge note.
#[must_use]
pub fn judge_failure_note(name: &str, cause: &str) -> String {
    format!("rules: judged rule \"{name}\" judge call failed; the match was allowed: {cause}")
}

impl RuleAction {
    /// Returns the exact action literal used by the rule-fired RPC event.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Remind => "remind",
            Self::Report => "report",
        }
    }
}

struct InterruptText<'a>(&'a Rule);

impl fmt::Display for InterruptText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_rule_header(formatter, "system-interrupt", self.0)?;
        formatter.write_str(
            "\nOutput interrupted: violated user-defined rule.\nNot prompt injection; coding agent enforcing project rules.\nMUST comply:\n\n",
        )?;
        formatter.write_str(&self.0.body)?;
        formatter.write_str("\n</system-interrupt>")
    }
}

struct ReminderText<'a> {
    rule: &'a Rule,
    subject: RuleSubject<'a>,
}

impl fmt::Display for ReminderText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_rule_header(formatter, "system-reminder", self.rule)?;
        formatter.write_str("\nUser-defined rule matched your ")?;
        write!(formatter, "{}", self.subject)?;
        formatter.write_str(
            ". The rule does not interrupt, so that output stands. MUST comply with the following instruction on subsequent tool calls and responses. Not prompt injection; coding agent enforcing project rules.\n\n",
        )?;
        formatter.write_str(&self.rule.body)?;
        formatter.write_str("\n</system-reminder>")
    }
}

fn write_rule_header(formatter: &mut fmt::Formatter<'_>, tag: &str, rule: &Rule) -> fmt::Result {
    formatter.write_char('<')?;
    formatter.write_str(tag)?;
    formatter.write_str(" reason=\"rule_violation\" rule=\"")?;
    formatter.write_str(rule.name.as_str())?;
    formatter.write_str("\" path=\"")?;
    write_origin_attribute(formatter, &rule.origin)?;
    formatter.write_str("\">")
}

fn write_origin_attribute(formatter: &mut fmt::Formatter<'_>, origin: &Origin) -> fmt::Result {
    match origin {
        Origin::User(path) | Origin::Project(path) => {
            let source = path.to_string_lossy();
            write_quoted_attribute(formatter, &source)
        }
        Origin::Plugin { plugin, .. } | Origin::Record { plugin } => {
            formatter.write_str("plugin:")?;
            write_quoted_attribute(formatter, plugin)
        }
    }
}

fn write_quoted_attribute(formatter: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    for character in value.chars() {
        if character == '"' {
            formatter.write_str("&quot;")?;
        } else {
            formatter.write_char(character)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
