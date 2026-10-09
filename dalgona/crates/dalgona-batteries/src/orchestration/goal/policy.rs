// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Continuation verdict: the pure P4 decision table, progress signatures,
//! goal-turn accounting, and mechanical blocks.

use super::super::StopKind;
use super::super::monitor::InflightCounts;
use super::sidecar::Goal;

/// Milliseconds after a user-started turn before a continuation may run.
pub(crate) const CONTINUATION_DELAY_MS: u64 = 10_000;

/// Mechanical block reason for the continuation cap.
pub(crate) const CAP_REASON: &str = "continuation cap reached";
/// Mechanical block reason for repeated assistant output.
pub(crate) const REPETITION_REASON: &str = "repeated assistant output";
/// Mechanical block reason for repeated truncation.
pub(crate) const LENGTH_REASON: &str = "output truncation repeated";
/// Mechanical block reason for the unattended limit.
pub(crate) const UNATTENDED_REASON: &str = "unattended continuation limit reached";
/// Mechanical block reason for an exhausted provider.
pub(crate) const PROVIDER_REASON: &str = "provider error ended the turn (retries exhausted)";
/// Mechanical block reason for an unrecovered context overflow.
pub(crate) const OVERFLOW_REASON: &str =
    "context overflow ended the turn (compaction did not recover)";

/// Consecutive continuations before the cap blocks.
pub(crate) const CAP_TURNS: u32 = 8;
/// Unattended continuations before the unattended limit blocks.
pub(crate) const UNATTENDED_TURNS: u32 = 150;
/// Consecutive tool-less goal turns before the stall notice.
pub(crate) const STALL_TURNS: u32 = 3;

/// Where a continuation decision runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GoalPath {
    /// A recovery turn after an interruption.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "goal recovery entry waits on durable goal sidecar persistence"
        )
    )]
    Recovery,
    /// The wake after a turn ended.
    AfterTurn,
    /// The grace window after a user-started turn.
    #[expect(
        dead_code,
        reason = "goal recovery entry waits on durable goal sidecar persistence"
    )]
    UserGrace,
    /// An idle wake with no turn behind it.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "goal recovery entry waits on durable goal sidecar persistence"
        )
    )]
    Idle,
}

/// Which prompt a granted continuation carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptKind {
    /// The full goal prompt with completion and blocked audits.
    Full,
    /// The minimal length-recovery prompt, used once.
    Minimal,
}

/// Why a continuation is denied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DenyReason {
    /// The last turn overflowed its context.
    ContextOverflow,
    /// The goal cannot continue on this path now.
    NotEligible,
    /// A continuation is already scheduled.
    SingleFlight,
    /// The last three outputs repeat.
    Repetition,
    /// Too many continuations without a user message.
    Unattended,
    /// Too many consecutive continuations on one signature.
    Cap,
    /// The progress signature did not move.
    Stale,
    /// A second length stop after one recovery.
    LengthExhausted,
}

impl DenyReason {
    /// Maps a deny to its stored mechanical block reason, if it blocks.
    /// `NotEligible`, `SingleFlight`, and `Stale` start nothing and change
    /// no state.
    #[must_use]
    pub(crate) fn mechanical_reason(self) -> Option<&'static str> {
        match self {
            Self::ContextOverflow => Some(OVERFLOW_REASON),
            Self::NotEligible | Self::SingleFlight | Self::Stale => None,
            Self::Repetition => Some(REPETITION_REASON),
            Self::Unattended => Some(UNATTENDED_REASON),
            Self::Cap => Some(CAP_REASON),
            Self::LengthExhausted => Some(LENGTH_REASON),
        }
    }
}

/// The pure continuation verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    /// Stay quiet for the deny reason.
    Deny(DenyReason),
    /// Schedule a continuation with one prompt kind.
    Continue { prompt: PromptKind, stall: bool },
}

/// Everything the verdict reads: the goal, the wake path, turn facts, the
/// progress signature, todo counts, and live inflight counts.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the continuation decision table fixes this input shape"
)]
pub(crate) struct VerdictInput<'a> {
    /// The goal under decision.
    pub(crate) goal: &'a Goal,
    /// Where the decision runs.
    pub(crate) path: GoalPath,
    /// Whether the session is idle.
    pub(crate) idle: bool,
    /// Whether a user message is pending.
    pub(crate) pending_user_messages: bool,
    /// Whether a continuation is already scheduled.
    pub(crate) continuation_pending: bool,
    /// Whether the last turn ended in context overflow.
    pub(crate) last_turn_context_overflow: bool,
    /// How the last turn stopped.
    pub(crate) last_stop: StopKind,
    /// Progress signature of the last assistant output.
    pub(crate) signature: &'a str,
    /// Open todo tasks from the session todo record.
    #[expect(
        dead_code,
        reason = "the continuation decision table fixes this input shape"
    )]
    pub(crate) open_todos: usize,
    /// Total todo tasks from the session todo record.
    #[expect(
        dead_code,
        reason = "the continuation decision table fixes this input shape"
    )]
    pub(crate) total_todos: usize,
    /// Live inflight counts from the session.
    #[expect(
        dead_code,
        reason = "the continuation decision table fixes this input shape"
    )]
    pub(crate) inflight: &'a InflightCounts,
}

/// Decides eligibility without consulting the deny table: active status and
/// no pending user message, plus the path's stop and idle requirements.
/// `Recovery` is eligible; `AfterTurn` and `UserGrace` require last stop
/// `Completed` or `Length`; `Idle` requires idle.
#[must_use]
pub(crate) fn eligible(input: &VerdictInput<'_>) -> bool {
    use super::super::GoalStatus;
    use StopKind::{Completed, Length};
    if input.goal.status != GoalStatus::Active || input.pending_user_messages {
        return false;
    }
    match input.path {
        GoalPath::Recovery => true,
        GoalPath::AfterTurn | GoalPath::UserGrace => {
            matches!(input.last_stop, Completed | Length)
        }
        GoalPath::Idle => input.idle,
    }
}

/// Counts the trailing run of equal output hashes.
#[must_use]
pub(crate) fn tail_streak(hashes: &[Box<str>]) -> usize {
    let Some(last) = hashes.last() else {
        return 0;
    };
    hashes.iter().rev().take_while(|hash| *hash == last).count()
}

/// Runs the pure decision table in plan order.
#[must_use]
pub(crate) fn verdict(input: &VerdictInput<'_>) -> Verdict {
    use super::super::GoalStatus;
    if input.goal.status == GoalStatus::Active && input.last_turn_context_overflow {
        return Verdict::Deny(DenyReason::ContextOverflow);
    }
    if !eligible(input) {
        return Verdict::Deny(DenyReason::NotEligible);
    }
    if input.continuation_pending {
        return Verdict::Deny(DenyReason::SingleFlight);
    }
    if tail_streak(&input.goal.recent_hashes) >= 3 {
        return Verdict::Deny(DenyReason::Repetition);
    }
    if input.goal.unattended >= UNATTENDED_TURNS {
        return Verdict::Deny(DenyReason::Unattended);
    }
    if input.goal.consecutive >= CAP_TURNS {
        return Verdict::Deny(DenyReason::Cap);
    }
    if input.path == GoalPath::AfterTurn
        && input.goal.last_signature.as_deref() == Some(input.signature)
    {
        return Verdict::Deny(DenyReason::Stale);
    }
    if input.last_stop == StopKind::Length {
        if input.goal.length_recoveries >= 1 {
            return Verdict::Deny(DenyReason::LengthExhausted);
        }
        return Verdict::Continue {
            prompt: PromptKind::Minimal,
            stall: input.goal.toolless_streak >= STALL_TURNS,
        };
    }
    Verdict::Continue {
        prompt: PromptKind::Full,
        stall: input.goal.toolless_streak >= STALL_TURNS,
    }
}

/// Normalizes assistant text: Unicode lowercase, each Unicode-whitespace run
/// to one ASCII space, trimmed.
fn normalize_text(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hashes UTF-8 bytes with 32-bit FNV-1a and renders eight lowercase hex
/// digits.
fn fnv_hex(bytes: &[u8]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// Builds one progress signature `<goal id>:<open todos>/<total todos>:<hash>`.
#[must_use]
pub(crate) fn progress_signature(
    goal_id: &str,
    open_todos: usize,
    total_todos: usize,
    last_assistant_text: &str,
) -> String {
    let hash = fnv_hex(normalize_text(last_assistant_text).as_bytes());
    format!("{goal_id}:{open_todos}/{total_todos}:{hash}")
}

/// Records one goal turn: delivery counters and signature on P4 arrival,
/// tool-less streak and output hash at turn end, plus turn usage. The output
/// hash history retains only the last three entries.
pub(crate) fn record_goal_turn(
    goal: &mut Goal,
    output_text: &str,
    tool_called: bool,
    tokens: u64,
    elapsed_seconds: u64,
    signature: &str,
    prompt: PromptKind,
) {
    if goal.last_signature.as_deref() != Some(signature) {
        goal.consecutive = 0;
    }
    goal.consecutive = goal.consecutive.saturating_add(1);
    goal.unattended = goal.unattended.saturating_add(1);
    goal.goal_turns = goal.goal_turns.saturating_add(1);
    goal.last_signature = Some(signature.into());
    if prompt == PromptKind::Minimal {
        goal.length_recoveries = goal.length_recoveries.saturating_add(1);
    } else {
        goal.length_recoveries = 0;
    }
    if tool_called {
        goal.toolless_streak = 0;
    } else {
        goal.toolless_streak = goal.toolless_streak.saturating_add(1);
    }
    let hash: Box<str> = fnv_hex(normalize_text(output_text).as_bytes()).into_boxed_str();
    goal.recent_hashes.push(hash);
    while goal.recent_hashes.len() > 3 {
        goal.recent_hashes.remove(0);
    }
    goal.tokens_used = goal.tokens_used.saturating_add(tokens);
    goal.time_used_s = goal.time_used_s.saturating_add(elapsed_seconds);
}

/// Reactivates a mechanically blocked goal on a user prompt and resets the
/// continuation counters. A nonmechanical block resumes only by `/goal
/// resume`.
pub(crate) fn on_user_prompt(goal: &mut Goal) {
    let mechanical = goal
        .blocked
        .as_ref()
        .is_some_and(|blocked| blocked.mechanical);
    if goal.status == super::super::GoalStatus::Blocked && mechanical {
        goal.status = super::super::GoalStatus::Active;
        goal.blocked = None;
        goal.consecutive = 0;
        goal.unattended = 0;
        goal.goal_turns = 0;
    }
}
