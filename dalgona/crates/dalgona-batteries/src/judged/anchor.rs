use dal_agent::ext::HookCx;
use dal_core::{ToolCallEvent, ToolCallVerdict};
use dal_ext::judge::{Gate, JudgeQuestion, Verdict};
use sonic_rs::JsonValueTrait;

use super::Battery;
use super::state::{CTX_SHARED_BYTES, FEATURE_ASK_ANCHOR, SHARED_TEXT_BYTES, cap_utf8};

pub(super) const ASK_TOOL: &str = "ask";
pub(super) const ASK_PROMPT: &str =
    "Classify the pending ask in the shared context. Pick the first option that applies.";
pub(super) const ASK_OPTIONS: [&str; 3] = ["explore-first", "ideal-state", "owner-decision"];
pub(super) const BLOCK_EXPLORE_FIRST: &str = "ask anchored: the evidence you already have can answer this. Explore first, and ask again only if it stays open.";
pub(super) const BLOCK_IDEAL_STATE: &str =
    "ask anchored: your goal state settles this. Take the resolving step instead of asking.";

fn is_explicit(raw: &str) -> bool {
    let Ok(args) = sonic_rs::from_str::<sonic_rs::Value>(raw) else {
        return false;
    };
    args.get("explicit")
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        == Some(true)
}

/// Classifies only `ask` calls that do not carry an explicit user request.
pub(super) async fn classify_tool_call(
    battery: &Battery,
    cx: &HookCx,
    event: &ToolCallEvent,
) -> ToolCallVerdict {
    if event.tool.as_str() != ASK_TOOL || !battery.cfg.ask_anchor {
        return ToolCallVerdict::Allow;
    }
    if is_explicit(event.args.as_str()) {
        return ToolCallVerdict::Allow;
    }
    let Some(judge) = battery.judge(cx) else {
        return ToolCallVerdict::Allow;
    };
    if !matches!(judge.state(), Gate::Ready { .. }) {
        return ToolCallVerdict::Allow;
    }

    let ask = cap_utf8(event.args.as_str(), SHARED_TEXT_BYTES);
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
    use super::is_explicit;

    #[test]
    fn explicit_true_bypasses_classification() {
        assert!(is_explicit(r#"{"explicit":true}"#));
    }

    #[test]
    fn missing_explicit_is_not_an_override() {
        assert!(!is_explicit(r#"{"question":"why"}"#));
    }

    #[test]
    fn malformed_arguments_fail_open() {
        assert!(!is_explicit("not json"));
    }
}
