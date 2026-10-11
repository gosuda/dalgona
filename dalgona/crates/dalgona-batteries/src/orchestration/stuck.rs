// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Pure loop-guard and sleep-wait policy reducers.
//!
//! The reducer splits at the plan's named seams: [`guard`] owns the
//! loop-guard state machine, `detect` owns the three detectors, [`sleep`]
//! owns the sleep-wait classifier, and [`rewrite`] owns the exec rewrite.
//! Exact plan literals live once in this facade so docs truth reads them
//! from one place.

use dal_core::Timestamp;

mod detect;
mod guard;
mod rewrite;
mod sleep;

#[cfg(test)]
mod tests;

/// Maximum tool-call records retained by the guard window.
const RECORD_CAPACITY: usize = 64;
pub(crate) use guard::{GuardState, GuardVerdict, clear_pending_attempts, on_tool_call, reset};
pub(crate) use rewrite::rewrite_exec_args;
pub(crate) use sleep::SleepClassifier;

pub(crate) const IDENTICAL_NOTICE: &str = "<system-reminder>\nLOOP GUARD - IDENTICAL TOOL CALLS: you called `<tool>` <count> times in a row with the EXACT same arguments. Re-issuing the same call returns the same result. Snap out of it:\n- reuse the result you already received from this exact call;\n- if you were re-checking for new output or state, use the monitor tool, or change one parameter deliberately (filter, offset, query);\n- if nothing is actually changing, stop calling this tool, state what is blocking you, and try a different tool or ask the user.\nDo not call `<tool>` again with identical arguments.\n</system-reminder>";
pub(crate) const SIMILAR_NOTICE: &str = "<system-reminder>\nLOOP GUARD - NEAR-IDENTICAL TOOL CALLS: your last <count> calls to `<tool>` had arguments about <percent>% identical. This may be legitimate batch work, or a loop that only looks like progress:\n- if these calls target different inputs, continue deliberately, but consider one wider call instead of one call per small variation;\n- if you are scanning output bit by bit, widen the window once, or use the monitor tool;\n- if the results keep saying the same thing, change strategy now: a different tool, a wider query, or asking the user.\n</system-reminder>";
pub(crate) const CYCLE_NOTICE_HEAD: &str = "<system-reminder>\nLOOP GUARD - REPEATING TOOL-CALL PATTERN: your recent tool calls repeat the cycle [";
pub(crate) const CYCLE_NOTICE_TAIL: &str = "] <count> times (period <period>). A fixed rotation usually means waiting, guessing, or searching without a discriminator:\n- if this is a wait-and-check rotation, stop it and use the monitor tool or wait for the job report;\n- if this is a search rotation, change one axis decisively: a broader query, a different tool, or a different source;\n- if each rotation moves distinct work forward, keep going, but say what each rotation achieved.\n</system-reminder>";
pub(crate) const BLOCKED_RECOVERY: &str = "Reuse the existing result, stop repeating this call, and re-plan from the current goal or choose a different tool.";
pub(crate) const POLL_RECOVERY: &str = "Stop polling this job. Its report arrives by itself when it ends; use the monitor tool to react to its output earlier.";
pub(crate) const LOOP_HARD_STOP_REASON: &str = "loop guard stopped the same call twice";
pub(crate) const LOOP_HARD_STOP_WARNING: &str =
    "Loop guard interrupted the turn after blocking <k> repeated calls to <tool>.";
pub(crate) const LOOP_P1_RECOVERY: &str = "The loop guard stopped the previous turn because you kept calling `<tool>` with arguments that had already been blocked. Do not repeat that call. Re-plan from the current goal and use a different tool or deliberately changed arguments.";

/// A deterministic guard-reducer failure (bad tool JSON, not a policy call).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct GuardError {
    message: Box<str>,
}

impl GuardError {
    pub(crate) fn json(error: impl core::fmt::Display) -> Self {
        Self {
            message: error.to_string().into(),
        }
    }
}

/// Substitutes `<key>` placeholders in an exact plan literal.
fn render_template(template: &str, values: &[(&str, &str)]) -> Box<str> {
    let mut rendered = template.to_owned();
    for (key, value) in values {
        rendered = rendered.replace(key, value);
    }
    rendered.into_boxed_str()
}

/// Renders ` · silent <minutes>m` once a job has been quiet for over ten
/// minutes. Returns `None` through exactly 600 seconds. Silence never
/// changes job or controller state.
///
/// The plan lists this helper in both the guard and monitor modules; this is
/// the single implementation, re-exported by the monitor module.
pub(crate) fn silence_suffix(last_activity: Timestamp, now: Timestamp) -> Option<String> {
    if now < last_activity {
        return None;
    }
    let seconds = now.as_second().checked_sub(last_activity.as_second())?;
    let elapsed = if now.subsec_nanosecond() < last_activity.subsec_nanosecond() {
        seconds.checked_sub(1)?
    } else {
        seconds
    };
    if elapsed <= 600 {
        return None;
    }
    let minutes = elapsed / 60;
    Some(format!(" · silent {minutes}m"))
}
