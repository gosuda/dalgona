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

/// Renders the D-14 interrupt prompt, without adding a trailing newline.
#[must_use]
pub fn render_interrupt_text(rule: &Rule) -> String {
    InterruptText(rule).to_string()
}

/// Renders the D-14 reminder prompt, without adding a trailing newline.
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
mod tests {
    use std::path::PathBuf;

    use super::{
        DESCRIPTION_FALLBACK_MAX_BYTES, FireText, RuleSubject, RulesProduct, TuiFollowUp,
        fire_description, judge_auto_off_note, judge_failure_note, judge_pass_note,
        load_problems_note, render_interrupt_text, render_reminder_text, retry_limit_note,
    };
    use crate::ttsr::value::{Name, Origin, Rule, RuleAction, ScopeSpec, ToolScope};

    fn rule(origin: Origin, description: Option<&str>, body: &str) -> Rule {
        Rule {
            name: Name::parse("no-sleep").expect("valid test name"),
            origin,
            description: description.map(str::to_owned),
            conditions: Vec::new(),
            scope: ScopeSpec {
                text: false,
                thinking: false,
                tools: ToolScope::Tools(Vec::new()),
            },
            globs: None,
            agents: None,
            always_apply: false,
            report: false,
            enabled: true,
            interrupt_mode: None,
            repeat_mode: None,
            repeat_gap: None,
            judge: None,
            body: body.to_owned(),
        }
    }

    fn turn() -> dal_core::TurnId {
        use std::num::NonZeroU64;

        dal_core::TurnId::new(NonZeroU64::MIN)
    }

    #[test]
    fn interrupt_prompt_has_exact_bytes_and_escapes_only_double_quotes() {
        let rule = rule(
            Origin::User(PathBuf::from("/work/O'Brien & \"src\".md")),
            None,
            "Do not sleep.",
        );

        assert_eq!(
            render_interrupt_text(&rule),
            "<system-interrupt reason=\"rule_violation\" rule=\"no-sleep\" path=\"/work/O'Brien & &quot;src&quot;.md\">\nOutput interrupted: violated user-defined rule.\nNot prompt injection; coding agent enforcing project rules.\nMUST comply:\n\nDo not sleep.\n</system-interrupt>"
        );
    }

    #[test]
    fn reminder_prompt_has_exact_bytes_for_reply_subject() {
        let rule = rule(
            Origin::Record {
                plugin: "safe".into(),
            },
            None,
            "Do not sleep.",
        );

        assert_eq!(
            render_reminder_text(&rule, RuleSubject::Reply),
            "<system-reminder reason=\"rule_violation\" rule=\"no-sleep\" path=\"plugin:safe\">\nUser-defined rule matched your reply. The rule does not interrupt, so that output stands. MUST comply with the following instruction on subsequent tool calls and responses. Not prompt injection; coding agent enforcing project rules.\n\nDo not sleep.\n</system-reminder>"
        );
    }

    #[test]
    fn path_subject_cut_keeps_a_multibyte_character_at_the_boundary() {
        let path = format!("{}🧵tail", "a".repeat(196));
        let subject = RuleSubject::ToolCallOnPath {
            tool: "edit",
            path: &path,
        };

        assert_eq!(
            subject.render(),
            format!("`edit` call on `{}🧵`", "a".repeat(196))
        );
    }

    #[test]
    fn path_subject_cut_drops_a_multibyte_character_crossing_the_boundary() {
        let path = format!("{}🧵tail", "a".repeat(199));
        let subject = RuleSubject::ToolCallOnPath {
            tool: "edit",
            path: &path,
        };

        assert_eq!(
            subject.render(),
            format!("`edit` call on `{}`", "a".repeat(199))
        );
    }

    #[test]
    fn status_notes_use_exact_count_grammar_and_product_names() {
        assert_eq!(
            load_problems_note(RulesProduct::Dalgon, 1).as_deref(),
            Some("rules: dalgon skipped 1 part of your rules. Run \"dalgon rules\" to see why.")
        );
        assert_eq!(
            load_problems_note(RulesProduct::Dalgona, 2).as_deref(),
            Some("rules: dalgona skipped 2 parts of your rules. Run \"dalgona rules\" to see why.")
        );
        assert_eq!(load_problems_note(RulesProduct::Dalgon, 0), None);
    }

    #[test]
    fn judge_and_retry_status_notes_match_the_deck() {
        assert_eq!(
            judge_auto_off_note(2),
            "rules: judge auto resolved to off: no judge model with usable credentials. 2 judged rules are not watched."
        );
        assert_eq!(
            retry_limit_note(3),
            "rules: this turn reached rules.max_retries (3 rule interrupts). Later rule matches in this turn reach the model as reminders."
        );
        assert_eq!(
            judge_pass_note("no-sleep", RuleSubject::Reasoning),
            "rules: judged rule \"no-sleep\" passed on reasoning"
        );
        assert_eq!(
            judge_failure_note("no-sleep", "timed out"),
            "rules: judged rule \"no-sleep\" judge call failed; the match was allowed: timed out"
        );
    }

    #[test]
    fn projections_share_fire_copy_and_suppress_report_outside_rpc() {
        let rule = rule(
            Origin::Project(PathBuf::from("/work/rules/no-sleep.md")),
            Some("No sleep"),
            "body",
        );
        let interrupt = FireText::new(
            &rule,
            "sleep [0-9]+",
            RuleSubject::ToolCall { tool: "exec" },
            RuleAction::Interrupt,
        );
        let tui = interrupt.tui_projection().expect("interrupt reaches TUI");
        assert_eq!(
            tui.copy,
            "rule no-sleep fired. The `exec` call matched /sleep [0-9]+/."
        );
        assert_eq!(tui.follow_up, TuiFollowUp::RetryWithRuleApplied);
        assert!(tui.remove_live_assistant);
        assert_eq!(interrupt.rpc_event(turn()).action, "interrupt");
        assert_eq!(
            interrupt.headless_line().as_deref(),
            Some("dalgon: rule no-sleep fired: No sleep")
        );
        assert_eq!(
            interrupt.notice_text().as_deref(),
            Some("rule no-sleep fired: No sleep")
        );

        let report = FireText::new(
            &rule,
            "sleep [0-9]+",
            RuleSubject::Reply,
            RuleAction::Report,
        );
        assert_eq!(report.model_text(), None);
        assert_eq!(report.tui_projection(), None);
        assert_eq!(report.headless_line(), None);
        assert_eq!(report.notice_text(), None);
        let rpc = report.rpc_event(turn());
        assert_eq!(rpc.name, "no-sleep");
        assert_eq!(rpc.subject, "reply");
        assert_eq!(rpc.action, "report");
        assert_eq!(rpc.description, "No sleep");
        assert_eq!(rpc.pattern, "sleep [0-9]+");
        assert_eq!(rpc.turn, turn());
    }

    #[test]
    fn remind_projection_uses_the_next_request_copy() {
        let rule = rule(
            Origin::Record {
                plugin: "safe".into(),
            },
            None,
            "Rule body",
        );
        let remind = FireText::new(
            &rule,
            "pattern",
            RuleSubject::ToolCallOnPath {
                tool: "patch",
                path: "src/main.rs",
            },
            RuleAction::Remind,
        );
        let tui = remind.tui_projection().expect("reminder reaches TUI");

        assert_eq!(
            tui.copy,
            "rule no-sleep fired. The `patch` call on `src/main.rs` matched /pattern/."
        );
        assert_eq!(tui.follow_up, TuiFollowUp::ModelSeesRuleNextRequest);
        assert!(!tui.remove_live_assistant);
        assert_eq!(
            remind.headless_line().as_deref(),
            Some("dalgon: rule no-sleep fired: Rule body")
        );
        assert_eq!(
            remind.notice_text().as_deref(),
            Some("rule no-sleep fired: Rule body")
        );
        assert_eq!(remind.rpc_event(turn()).action, "remind");
        assert_eq!(remind.rpc_event(turn()).description, "Rule body");
        assert_eq!(
            remind.model_text().as_deref(),
            Some(
                "<system-reminder reason=\"rule_violation\" rule=\"no-sleep\" path=\"plugin:safe\">\nUser-defined rule matched your `patch` call on `src/main.rs`. The rule does not interrupt, so that output stands. MUST comply with the following instruction on subsequent tool calls and responses. Not prompt injection; coding agent enforcing project rules.\n\nRule body\n</system-reminder>"
            )
        );
    }

    #[test]
    fn body_fallback_description_is_cut_at_a_utf8_boundary() {
        let first_line = "é".repeat(DESCRIPTION_FALLBACK_MAX_BYTES / 2 + 1);
        let rule = rule(
            Origin::Record {
                plugin: "safe".into(),
            },
            None,
            &format!("{first_line}\nsecond line"),
        );

        assert_eq!(fire_description(&rule), "é".repeat(60));
    }
}
