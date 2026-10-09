// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use dal_agent::ext::HookCx;
use dal_core::Settled;
use dal_core::ext::BeforeTurn;
use dal_ext::judge::{Gate, JudgeQuestion, Verdict};

use super::Battery;
use super::dedup::Admission;
use super::state::{
    CHANNEL_CLAIM_CHECK, CLAIM_MIN_REPLY_BYTES, CLAIM_REMINDER, CLAIM_REMINDER_BYTES,
    FEATURE_CLAIM_CHECK, SHARED_TEXT_BYTES, cap_utf8,
};

pub(super) const CLAIM_PROMPT: &str =
    "Does the reply in the shared context assert unverified facts that the next step would act on?";

/// Checks a settled top-level reply and parks at most one reminder per session.
pub(super) async fn on_settled(battery: &Battery, cx: &HookCx, event: &Settled) {
    let (reply, judge, is_child) = battery.with_session(cx.session, |state| {
        state.turn_no = state.turn_no.saturating_add(1);
        (
            state.digest.assistant_text().to_owned(),
            state.judge.clone(),
            state.is_child,
        )
    });
    if !battery.cfg.claim_check || is_child || reply.len() < CLAIM_MIN_REPLY_BYTES {
        return;
    }
    let Some(judge) = judge else {
        return;
    };
    if !matches!(judge.state(), Gate::Ready { .. }) {
        return;
    }

    let shared = format!(
        "Reply:\n<<<REPLY\n{}\nREPLY>>>",
        cap_utf8(&reply, SHARED_TEXT_BYTES)
    );
    let Ok(question) = JudgeQuestion::bool(CLAIM_PROMPT) else {
        return;
    };
    if !matches!(
        judge
            .judge(
                FEATURE_CLAIM_CHECK,
                &shared,
                question,
                Some(event.turn),
                None,
            )
            .await,
        Ok(Verdict::Bool(true))
    ) {
        return;
    }

    let reminder = cap_utf8(CLAIM_REMINDER, CLAIM_REMINDER_BYTES).to_owned();
    let outstanding = battery.with_session(cx.session, |state| {
        if let Some(parked) = &state.parked {
            Some(parked.clone())
        } else {
            state.parked = Some(reminder.clone());
            None
        }
    });
    if let Some(old) = outstanding {
        let _ = battery
            .consult(cx.session, &old, &reminder, Some(event.turn), &judge)
            .await;
    }
}

/// Admits and delivers one parked reminder to the next turn's user context.
pub(super) async fn on_before_turn(
    battery: &Battery,
    cx: &HookCx,
    _event: &BeforeTurn,
) -> Option<String> {
    let (parked, judge) = battery.with_session(cx.session, |state| {
        Some((state.parked.clone()?, state.judge.clone()?))
    })?;
    match battery
        .admit(cx.session, CHANNEL_CLAIM_CHECK, &parked, cx.turn, &judge)
        .await
    {
        Admission::Refuse => {
            battery.with_session(cx.session, |state| {
                if state.parked.as_deref() == Some(parked.as_str()) {
                    state.parked = None;
                }
            });
            None
        }
        Admission::Admit => {
            let delivered = battery.with_session(cx.session, |state| {
                if state.parked.as_deref() != Some(parked.as_str()) {
                    return false;
                }
                state.parked = None;
                state.record_injection(CHANNEL_CLAIM_CHECK, &parked);
                true
            });
            delivered.then_some(parked)
        }
    }
}
