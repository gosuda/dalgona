//! Fire action resolution: rule mode, budget, and source class to action.
//!
//! A `report` rule always reports. Under a reminders-only budget a judged
//! rule reports too, because its interrupt would otherwise stop the stream
//! for a verdict the caller never asked to enforce. Otherwise the rule's
//! own interrupt mode, or the configured default, decides against the
//! source the match landed on.

use super::super::texts::RuleSubject;
use super::super::value::{InterruptMode, Rule, RuleAction};
use super::{SourceKind, WatchBudget};

pub(super) fn action_of(
    rule: &Rule,
    source: &SourceKind,
    budget: WatchBudget,
    default: InterruptMode,
) -> RuleAction {
    if rule.report {
        return RuleAction::Report;
    }
    if rule.judge.is_some() && budget == WatchBudget::RemindersOnly {
        return RuleAction::Report;
    }
    let mode = rule.interrupt_mode.unwrap_or(default);
    let interrupts = budget == WatchBudget::Interrupts && admits(mode, source);
    if interrupts {
        RuleAction::Interrupt
    } else {
        RuleAction::Remind
    }
}

fn admits(mode: InterruptMode, source: &SourceKind) -> bool {
    match mode {
        InterruptMode::Always => true,
        InterruptMode::ProseOnly => {
            matches!(source, SourceKind::Text | SourceKind::Thinking)
        }
        InterruptMode::ToolOnly => matches!(source, SourceKind::Tool { .. }),
        // The core enum is non-exhaustive; an unknown future mode admits
        // nothing.
        _ => false,
    }
}
pub(super) fn subject_of<'a>(source: &'a SourceKind, path: Option<&'a str>) -> RuleSubject<'a> {
    match (source, path) {
        (SourceKind::Text, _) => RuleSubject::Reply,
        (SourceKind::Thinking, _) => RuleSubject::Reasoning,
        (SourceKind::Tool { tool }, None) => RuleSubject::ToolCall { tool },
        (SourceKind::Tool { tool }, Some(path)) => RuleSubject::ToolCallOnPath { tool, path },
    }
}
