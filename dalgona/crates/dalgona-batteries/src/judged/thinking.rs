use dal_agent::ext::HookCx;
use dal_core::ext::BeforeRequest;
use dal_core::{Notice, RequestParams, ThinkingLevel};
use dal_ext::judge::{Gate, JudgeError, JudgeQuestion, Verdict};

use super::Battery;
use super::state::{BREAKER_NOTICE, CLASSIFY_WAIT_MS, FEATURE_THINKING, THINKING_BREAKER_STREAK};

pub(super) const CLASSIFY_PROMPT: &str = "How much reasoning does this turn need? Pick the smallest level that fits; pick the current default, medium, when unsure.";

/// Classifies the first request and reapplies its chosen level to later rounds.
pub(super) async fn classify(
    battery: &Battery,
    cx: &HookCx,
    event: &BeforeRequest,
) -> Option<RequestParams> {
    if event.round != 0 {
        return battery.with_session(cx.session, |state| {
            let (turn, level) = state.turn_thinking?;
            (turn == event.turn).then(|| {
                let mut params = event.params.clone();
                params.thinking = level;
                params
            })
        });
    }

    let judge = battery.ensure_judge(cx).await?;
    if !battery.cfg.thinking || !matches!(judge.state(), Gate::Ready { .. }) || cx.parent.is_some()
    {
        return None;
    }

    let user = battery.with_session(cx.session, |state| {
        if event.thinking_explicit {
            state.latch.observe_user_level();
        }
        if !state.latch.classification_armed() {
            return None;
        }
        if state.digest.user_text().is_empty() || event.caps.thinking.len() < 2 {
            return None;
        }
        Some(state.digest.user_text().to_owned())
    })?;
    let options: Vec<&str> = event
        .caps
        .thinking
        .iter()
        .map(ThinkingLevel::name)
        .collect();
    let shared = format!("User turn:\n<<<TURN\n{user}\nTURN>>>");
    let Ok(question) = JudgeQuestion::choice(CLASSIFY_PROMPT, &options) else {
        return None;
    };
    let verdict = judge
        .judge(
            FEATURE_THINKING,
            &shared,
            question,
            Some(event.turn),
            Some(CLASSIFY_WAIT_MS),
        )
        .await;

    let (params, fire_notice) = battery.with_session(cx.session, |state| match verdict {
        Ok(Verdict::Choice(index)) => {
            let Some(level) = event.caps.thinking.get(usize::from(index)).copied() else {
                return (None, false);
            };
            state.breaker_streak = 0;
            state.turn_thinking = Some((event.turn, level));
            let mut params = event.params.clone();
            params.thinking = level;
            (Some(params), false)
        }
        Err(
            JudgeError::Timeout { .. } | JudgeError::Parse { .. } | JudgeError::Provider { .. },
        ) => {
            state.breaker_streak = state.breaker_streak.saturating_add(1);
            let fire_notice =
                state.breaker_streak == THINKING_BREAKER_STREAK && !state.breaker_notice_sent;
            if fire_notice {
                state.breaker_notice_sent = true;
                state.latch.observe_breaker();
            }
            (None, fire_notice)
        }
        _ => (None, false),
    });
    if fire_notice {
        cx.services.notify(
            &cx.caller,
            Notice {
                turn: None,
                kind: Box::from("judged"),
                text: BREAKER_NOTICE.into(),
            },
        );
    }
    params
}
