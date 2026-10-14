// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use dal_agent::ext::HookCx;
use dal_core::{ToolCallEvent, ToolCallVerdict};
use dal_ext::judge::{Gate, JudgeQuestion, Verdict};
use sonic_rs::JsonValueTrait;

use super::Battery;
use super::state::{CTX_SHARED_BYTES, FEATURE_ASK_ANCHOR, SHARED_TEXT_BYTES};
use crate::text::byte_prefix;

pub(super) const ASK_TOOL: &str = "ask";
pub(super) const ASK_PROMPT: &str =
    "Classify the pending ask in the shared context. Pick the first option that applies.";
pub(super) const ASK_OPTIONS: [&str; 3] = ["explore-first", "ideal-state", "owner-decision"];
pub(super) const BLOCK_EXPLORE_FIRST: &str = "ask anchored: the evidence you already have can answer this. Explore first, and ask again only if it stays open.";
pub(super) const BLOCK_IDEAL_STATE: &str =
    "ask anchored: your goal state settles this. Take the resolving step instead of asking.";

/// Whether `raw` is valid JSON that does not carry `explicit = true`. Only
/// such a call is classified: an explicit ask is the user's own question, and
/// arguments that are not valid JSON are never judged.
fn needs_classification(raw: &str) -> bool {
    let Ok(args) = sonic_rs::from_str::<sonic_rs::Value>(raw) else {
        return false;
    };
    args.get("explicit").and_then(JsonValueTrait::as_bool) != Some(true)
}

/// Classifies only `ask` calls that do not carry an explicit user request.
///
/// Every path that does not reach a judge verdict allows the call: another
/// tool, the toggle off, a child session (children never ask), malformed or
/// explicit arguments, a missing or not-ready judge, and any judge failure.
pub(super) async fn classify_tool_call(
    battery: &Battery,
    cx: &HookCx,
    event: &ToolCallEvent,
) -> ToolCallVerdict {
    if event.tool.as_str() != ASK_TOOL || !battery.cfg.ask_anchor || cx.parent.is_some() {
        return ToolCallVerdict::Allow;
    }
    if !needs_classification(event.args.as_str()) {
        return ToolCallVerdict::Allow;
    }
    let Some(judge) = battery.judge(cx) else {
        return ToolCallVerdict::Allow;
    };
    if !matches!(judge.state(), Gate::Ready { .. }) {
        return ToolCallVerdict::Allow;
    }

    let ask = byte_prefix(event.args.as_str(), SHARED_TEXT_BYTES);
    let context = battery.with_session(cx.session, |state| state.digest.render(CTX_SHARED_BYTES));
    let shared =
        format!("Ask call:\n<<<ASK\n{ask}\nASK>>>\n\nRecent context:\n<<<CTX\n{context}\nCTX>>>");
    let Ok(question) = JudgeQuestion::choice(ASK_PROMPT, &ASK_OPTIONS) else {
        return ToolCallVerdict::Allow;
    };

    match judge
        .judge(
            FEATURE_ASK_ANCHOR,
            &shared,
            question,
            Some(event.turn),
            None,
        )
        .await
    {
        Ok(Verdict::Choice(0)) => ToolCallVerdict::Block {
            reason: Box::from(BLOCK_EXPLORE_FIRST),
        },
        Ok(Verdict::Choice(1)) => ToolCallVerdict::Block {
            reason: Box::from(BLOCK_IDEAL_STATE),
        },
        _ => ToolCallVerdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::needs_classification;

    #[test]
    fn explicit_true_bypasses_classification() {
        assert!(!needs_classification(r#"{"explicit":true}"#));
    }

    #[test]
    fn missing_explicit_is_classified() {
        assert!(needs_classification(r#"{"question":"why"}"#));
    }

    #[test]
    fn explicit_false_and_non_boolean_values_are_classified() {
        assert!(needs_classification(r#"{"explicit":false}"#));
        assert!(needs_classification(r#"{"explicit":"true"}"#));
        assert!(needs_classification(r#"{"explicit":null}"#));
    }

    #[test]
    fn malformed_arguments_are_never_classified() {
        assert!(!needs_classification("not json"));
        assert!(!needs_classification(""));
    }
}
