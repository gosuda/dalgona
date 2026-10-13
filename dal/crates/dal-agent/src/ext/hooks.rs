//! Typed hook dispatch for the extension runtime.
//!
//! The generation owns the canonical hook order and hands this module
//! already-ordered, per-extension hook slices; dispatch never reorders.
//! Every guard dispatch applies one effective deadline and the turn
//! cancellation token, and every failure resolves fail-closed for its
//! event. Observer delivery never blocks the loop: lossy queues shed the
//! oldest event under flood, lossless queues apply cancellable
//! backpressure, and observer failures are counted, never raised.
//!
//! Submodules: [`observe`] owns observer queues and observe-only dispatch;
//! [`watch`] owns the stream watcher contract and per-request delivery.

pub mod observe;
pub mod watch;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use crate::ext::ObserverError;
use dal_core::ext::{BeforeRequest, BeforeTurn};
use dal_core::{
    HookEvent, InputEvent, InputVerdict, RUST_STREAM_EVENT, RawJson, RequestParams, SessionId,
    ToolCallEvent, ToolCallVerdict, TurnId,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::ext::generation::Generation;
use crate::ext::{
    BoxFuture, Caller, CallerKind, Extension, Hook, HookCx, HookError, Name, ScriptCx, Services,
};

pub use dal_core::{Caps, Part, ThinkingLevel};
pub use observe::{
    LosslessQueue, LossyQueue, ObserverReport, dispatch_session_end, dispatch_session_start,
    dispatch_settled, dispatch_tool_result, dispatch_turn_end,
};
pub use watch::{
    Channel, GateSeed, MAX_STREAM_INTERRUPTS, StreamFire, StreamFireAction, StreamVerdict,
    StreamWatch, StreamWatchRecord, TurnInfo, WatchBudget, WatchFactory, feed_watchers,
    finish_watchers, interrupts_capped, start_watchers,
};

/// Per-session observer queue bound; the loop owns event production.
pub const OBSERVER_QUEUE_CAPACITY: usize = 256;
/// Lossless observer queue bound; a full queue waits on space or cancel.
pub const LOSSLESS_OBSERVER_QUEUE_CAPACITY: usize = 64;
/// Guard hook budget (`hooks_deadline = "5s"`) in milliseconds.
pub const HOOK_DEADLINE_MS: u64 = 5000;

/// Injected approval answer for final tool-call arguments; the actor wires
/// the real broker, tests inject scripted approvers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Approval {
    /// Run the call with the final arguments.
    Allow,
    /// Refuse the call with a model-facing reason.
    Block {
        /// Why the call was refused.
        reason: Box<str>,
    },
    /// The approval request itself was denied or abandoned.
    Denied,
}

/// Settled outcome of one tool call after hooks and approval.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolDecision {
    /// Run the call with these final arguments.
    Allow {
        /// Final arguments after every rewrite.
        args: RawJson,
    },
    /// Refuse the call with a model-facing reason.
    Block {
        /// Hook block, hook failure, or approver refusal text.
        reason: Box<str>,
    },
    /// Approval was denied or abandoned; the actor maps this to its
    /// fail-closed denial text.
    Denied,
}

/// One extension's share of a `tool_call` fold.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallStep {
    /// Arguments after this extension's rewrites.
    pub args: RawJson,
    /// `Some` stops the whole chain with this block reason.
    pub block: Option<Box<str>>,
}

/// One extension's share of an `input` fold.
#[derive(Clone, Debug, PartialEq)]
pub struct InputStep {
    /// Content after this extension's transforms.
    pub content: Vec<Part>,
    /// `true` ends dispatch with handled content.
    pub stop: bool,
    /// One notice per failed hook, in hook order.
    pub notices: Vec<Box<str>>,
}

/// One extension's share of a `before_turn` fold.
#[derive(Clone, Debug, PartialEq)]
pub struct BeforeTurnStep {
    /// Non-empty texts contributed by this extension, in hook order.
    pub texts: Vec<Box<str>>,
    /// One notice per failed hook, in hook order.
    pub notices: Vec<Box<str>>,
}

/// One extension's share of a `before_request` fold.
#[derive(Clone, Debug, PartialEq)]
pub struct BeforeRequestStep {
    /// Parameters after this extension's replacements.
    pub params: RequestParams,
    /// One notice per failed hook, in hook order.
    pub notices: Vec<Box<str>>,
}

/// Caller-owned context shared by one per-extension dispatch call.
pub struct DispatchCx<'a> {
    /// Host-minted identity of the calling extension.
    pub caller: &'a Caller,
    /// Host services for hook contexts.
    pub services: &'a Arc<dyn Services>,
    /// Session that owns the event.
    pub session: SessionId,
    /// Parent session when the event runs in a subagent session.
    pub parent: Option<SessionId>,
    /// Host-captured process-edge environment snapshot.
    pub process_env: Arc<crate::Env>,
    /// Turn that owns the event, when there is one.
    pub turn: Option<TurnId>,
    /// Turn cancellation token bounding every hook wait.
    pub cancel: &'a CancellationToken,
    /// Turn deadline; each dispatch additionally applies the hook budget.
    pub turn_deadline: Instant,
    /// The captured script seam of a scripted hook, absent otherwise (E06).
    pub script: Option<ScriptCx>,
}

/// Minted hook caller for one extension; `None` when the extension's name
/// cannot parse (a malformed generation entry is skipped, not fatal).
#[must_use]
pub(crate) fn hook_caller(extension: &Extension, turn: Option<TurnId>) -> Option<Caller> {
    let name = extension.name().parse::<Name>().ok()?;
    Some(Caller::new(
        name,
        extension.origin(),
        extension.inject(),
        extension.state_version(),
        CallerKind::Hook,
        turn,
    ))
}

/// The fields one hook fan-out shares: everything [`DispatchCx`] carries
/// except the per-extension caller and the event's turn. [`Self::for_each`]
/// mints each extension's caller and context in registration order.
pub(crate) struct HookScope<'a> {
    /// Host services for hook contexts.
    pub services: &'a Arc<dyn Services>,
    /// Session that owns the event.
    pub session: SessionId,
    /// Parent session when the event runs in a subagent session.
    pub parent: Option<SessionId>,
    /// Host-captured process-edge environment snapshot.
    pub process_env: Arc<crate::Env>,
    /// Cancellation token bounding every hook wait.
    pub cancel: &'a CancellationToken,
    /// Turn deadline; each dispatch additionally applies the hook budget.
    pub turn_deadline: Instant,
    /// The captured script seam of a scripted hook, absent otherwise (E06).
    pub script: Option<ScriptCx>,
}

impl HookScope<'_> {
    /// Builds the dispatch context of one iteration over
    /// [`hook_fanout`]: `caller` is that iteration's minted caller and
    /// `turn` is the turn passed to `hook_fanout`.
    pub(crate) fn cx<'a>(&'a self, caller: &'a Caller, turn: Option<TurnId>) -> DispatchCx<'a> {
        DispatchCx {
            caller,
            services: self.services,
            session: self.session,
            parent: self.parent,
            process_env: Arc::clone(&self.process_env),
            turn,
            cancel: self.cancel,
            turn_deadline: self.turn_deadline,
            script: self.script.clone(),
        }
    }
}

/// Iterates the generation's extensions in canonical registration order,
/// yielding each extension's index, record, and minted hook caller.
/// Extensions whose names cannot parse are skipped.
pub(crate) fn hook_fanout(
    generation: &Generation,
    turn: Option<TurnId>,
) -> impl Iterator<Item = (usize, &Extension, Caller)> + '_ {
    generation
        .extensions
        .iter()
        .enumerate()
        .filter_map(move |(index, extension)| {
            Some((index, extension, hook_caller(extension, turn)?))
        })
}

impl DispatchCx<'_> {
    /// The single effective deadline shared by every `HookCx` minted here.
    fn deadline(&self) -> Instant {
        effective_deadline(self.turn_deadline, HOOK_DEADLINE_MS)
    }

    /// Mints one `HookCx` for a single hook invocation under `deadline`.
    /// The caller binds one deadline per dispatch and passes it for every
    /// hook, so the budget never restarts mid-chain.
    fn mint(&self, deadline: Instant) -> HookCx {
        mint_cx(
            self.caller.clone(),
            Arc::clone(self.services),
            self.session,
            self.parent,
            Arc::clone(&self.process_env),
            self.turn,
            self.cancel.clone(),
            deadline,
            self.script.clone(),
        )
    }
}
/// The one effective deadline for hook invocation: the earlier of the turn
/// deadline and now plus `wait_ms`. Pass [`HOOK_DEADLINE_MS`] for guard
/// hooks; pass the grant wait for approval waits.
#[must_use]
pub fn effective_deadline(turn_deadline: Instant, wait_ms: u64) -> Instant {
    let budget = Instant::now() + std::time::Duration::from_millis(wait_ms);
    if budget < turn_deadline {
        budget
    } else {
        turn_deadline
    }
}

/// Mints one [`HookCx`] for a single hook invocation of `caller`.
#[must_use]
pub fn mint_cx(
    caller: Caller,
    services: Arc<dyn Services>,
    session: SessionId,
    parent: Option<SessionId>,
    process_env: Arc<crate::Env>,
    turn: Option<TurnId>,
    cancel: CancellationToken,
    deadline: Instant,
    script: Option<ScriptCx>,
) -> HookCx {
    HookCx {
        caller,
        services,
        process_env,
        session,
        parent,
        turn,
        cancel,
        deadline,
        script,
    }
}

/// Fail-closed block text for a failed guard hook; the error displays the
/// message verbatim: `hook "<ext>" failed: <cause>`.
#[must_use]
pub fn hook_block_message(ext: &str, err: &HookError) -> Box<str> {
    format!(r#"hook "{ext}" failed: {err}"#).into()
}

/// Fail-closed notice text naming the extension, event, and outcome.
#[must_use]
pub fn hook_notice(ext: &str, event: &str, what_happened: &str) -> Box<str> {
    format!(r#"plugin "{ext}" hook {event} failed; {what_happened}"#).into()
}

/// Notice outcome for a failed `input` hook.
pub const NOTICE_INPUT_UNCHANGED: &str = "input continued unchanged";
/// Notice outcome for a failed `before_turn` hook.
pub const NOTICE_BEFORE_TURN_SKIPPED: &str = "before_turn skipped";
/// Notice outcome for a failed `before_request` hook.
pub const NOTICE_BEFORE_REQUEST_IGNORED: &str = "before_request ignored";
/// Notice outcome when a stream watcher is dropped.
pub const NOTICE_WATCHER_DROPPED: &str = "stream watcher dropped";

/// Notice naming the `input` event literal.
#[must_use]
pub fn input_failed_notice(ext: &str) -> Box<str> {
    hook_notice(ext, HookEvent::Input.as_str(), NOTICE_INPUT_UNCHANGED)
}

/// Notice naming the `before_turn` event literal.
#[must_use]
pub fn before_turn_failed_notice(ext: &str) -> Box<str> {
    hook_notice(
        ext,
        HookEvent::BeforeTurn.as_str(),
        NOTICE_BEFORE_TURN_SKIPPED,
    )
}

/// Notice naming the `before_request` event literal.
#[must_use]
pub fn before_request_failed_notice(ext: &str) -> Box<str> {
    hook_notice(
        ext,
        HookEvent::BeforeRequest.as_str(),
        NOTICE_BEFORE_REQUEST_IGNORED,
    )
}

/// Notice naming the Rust-only stream watcher event.
#[must_use]
pub fn watcher_dropped_notice(ext: &str) -> Box<str> {
    hook_notice(ext, RUST_STREAM_EVENT, NOTICE_WATCHER_DROPPED)
}

/// Rank of a reasoning level for cap clamping, in `caps.thinking` order.
const fn thinking_rank(level: ThinkingLevel) -> u8 {
    match level {
        ThinkingLevel::Off => 0,
        ThinkingLevel::Minimal => 1,
        ThinkingLevel::Low => 2,
        ThinkingLevel::Medium => 3,
        ThinkingLevel::High => 4,
        ThinkingLevel::Xhigh => 5,
        ThinkingLevel::Max => 6,
    }
}

/// Clamps typed request parameters to the model caps. A requested thinking
/// level outside `caps.thinking` falls back to the highest supported level
/// at or below it, else the lowest supported level. Temperature, effort,
/// and token caps have no model-cap range in `Caps` and pass through
/// unchanged; raw payload bytes are never touched here.
#[must_use]
pub fn clamp_params(params: RequestParams, caps: &Caps) -> RequestParams {
    if caps.thinking.is_empty() || caps.thinking.contains(&params.thinking) {
        return params;
    }
    let want = thinking_rank(params.thinking);
    let mut below: Option<ThinkingLevel> = None;
    let mut floor: Option<ThinkingLevel> = None;
    for level in caps.thinking.iter().copied() {
        let rank = thinking_rank(level);
        if rank <= want && below.is_none_or(|best| rank > thinking_rank(best)) {
            below = Some(level);
        }
        if floor.is_none_or(|best| rank < thinking_rank(best)) {
            floor = Some(level);
        }
    }
    let thinking = below.or(floor).unwrap_or(params.thinking);
    RequestParams { thinking, ..params }
}

/// Runs one hook future under the effective deadline and the turn cancel
/// token. Timeout resolves to an over-budget failure; cancel resolves to
/// [`HookError::Cancelled`]; both stay fail-closed at the call site.
async fn settle<T>(
    future: BoxFuture<'static, Result<T, HookError>>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<T, HookError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(HookError::Cancelled),
        outcome = tokio::time::timeout_at(deadline, future) => match outcome {
            Ok(result) => result,
            Err(_) => Err(HookError::Failed { message: "over budget".into() }),
        },
    }
}

/// Folds one extension's `tool_call` hooks over `args` in registration
/// order. Rewrites feed the next hook; the first block ends this share;
/// any hook failure blocks with `hook "<ext>" failed: <cause>`. The caller
/// threads [`ToolCallStep::args`] through extensions in canonical order,
/// stops at the first [`ToolCallStep::block`], and runs approval once on
/// the final arguments via [`approve_tool_call`].
pub async fn dispatch_tool_call(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>],
    event: &ToolCallEvent,
    args: RawJson,
) -> ToolCallStep {
    let deadline = cx.deadline();
    let mut current = args;
    for hook in hooks {
        let input = ToolCallEvent {
            args: current.clone(),
            ..event.clone()
        };
        match settle(hook.call(input, cx.mint(deadline)), cx.cancel, deadline).await {
            Ok(ToolCallVerdict::Allow) => {}
            Ok(ToolCallVerdict::Rewrite { args }) => current = args,
            Ok(ToolCallVerdict::Block { reason }) => {
                return ToolCallStep {
                    args: current,
                    block: Some(reason),
                };
            }
            Ok(_) => {
                let err = HookError::Failed {
                    message: "unknown verdict".into(),
                };
                record_observer_failure(cx, ext, &err);
                return ToolCallStep {
                    args: current,
                    block: Some(hook_block_message(ext, &err)),
                };
            }
            Err(err) => {
                record_observer_failure(cx, ext, &err);
                return ToolCallStep {
                    args: current,
                    block: Some(hook_block_message(ext, &err)),
                };
            }
        }
    }
    ToolCallStep {
        args: current,
        block: None,
    }
}

/// Records one guard-hook failure against the enclosing invocation's root
/// when a script seam is active; the triggering operation keeps its
/// outcome either way (R07 P05).
fn record_observer_failure(cx: &DispatchCx<'_>, ext: &str, err: &HookError) {
    let Some(script) = &cx.script else {
        return;
    };
    let Some(parent) = &script.parent else {
        return;
    };
    let Ok(ext) = ext.parse::<Name>() else {
        return;
    };
    script.host.observe_failed(
        parent,
        ObserverError {
            ext,
            event: HookEvent::ToolCall,
            message: err.to_string().into(),
        },
    );
}

/// Runs the injected approver once on the final tool-call arguments.
/// Cancel or the effective deadline resolves fail-closed to [`ToolDecision::Denied`].
pub async fn approve_tool_call(
    event: &ToolCallEvent,
    approver: &dyn Fn(ToolCallEvent) -> BoxFuture<'static, Approval>,
    cancel: &CancellationToken,
    turn_deadline: Instant,
    grant_wait_ms: u64,
) -> ToolDecision {
    let deadline = effective_deadline(turn_deadline, grant_wait_ms);
    tokio::select! {
        biased;
        () = cancel.cancelled() => ToolDecision::Denied,
        () = tokio::time::sleep_until(deadline) => ToolDecision::Denied,
        decision = approver(event.clone()) => match decision {
            Approval::Allow => ToolDecision::Allow { args: event.args.clone() },
            Approval::Block { reason } => ToolDecision::Block { reason },
            Approval::Denied => ToolDecision::Denied,
        },
    }
}
/// Folds one extension's `input` hooks over the content in registration
/// order. Transforms feed the next hook; a failure keeps the running
/// content with one notice. A `builtin` handled result still lets later
/// origins run; the first later handled result stops dispatch.
pub async fn dispatch_input(
    ext: &str,
    cx: &DispatchCx<'_>,
    builtin: bool,
    hooks: &[Arc<dyn Hook<InputEvent, InputVerdict>>],
    event: &InputEvent,
) -> InputStep {
    let deadline = cx.deadline();
    let mut current = event.content.clone();
    let mut notices = Vec::new();
    for hook in hooks {
        let input = InputEvent {
            content: current.clone(),
        };
        match settle(hook.call(input, cx.mint(deadline)), cx.cancel, deadline).await {
            Ok(InputVerdict::Continue) => {}
            Ok(InputVerdict::Transform(parts)) => current = parts,
            Ok(InputVerdict::Handled) => {
                if builtin {
                    continue;
                }
                return InputStep {
                    content: current,
                    stop: true,
                    notices,
                };
            }
            Ok(_) | Err(_) => notices.push(input_failed_notice(ext)),
        }
    }
    InputStep {
        content: current,
        stop: false,
        notices,
    }
}

/// Collects one extension's `before_turn` texts in hook order. A failure
/// adds nothing with one notice. The caller joins every extension's texts
/// with [`join_before_turn`].
pub async fn dispatch_before_turn(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn Hook<BeforeTurn, Option<String>>>],
    event: &BeforeTurn,
) -> BeforeTurnStep {
    let deadline = cx.deadline();
    let mut texts = Vec::new();
    let mut notices = Vec::new();
    for hook in hooks {
        match settle(
            hook.call(event.clone(), cx.mint(deadline)),
            cx.cancel,
            deadline,
        )
        .await
        {
            Ok(Some(text)) => {
                if !text.is_empty() {
                    texts.push(text.into_boxed_str());
                }
            }
            Ok(None) => {}
            Err(_) => notices.push(before_turn_failed_notice(ext)),
        }
    }
    BeforeTurnStep { texts, notices }
}

/// Joins non-empty `before_turn` texts in canonical order with one blank
/// line. Returns `None` when no hook contributed text.
#[must_use]
pub fn join_before_turn(texts: &[Box<str>]) -> Option<String> {
    let mut kept: Vec<&str> = Vec::new();
    for text in texts {
        if !text.is_empty() {
            kept.push(text);
        }
    }
    if kept.is_empty() {
        None
    } else {
        Some(kept.join("\n\n"))
    }
}

/// Folds one extension's `before_request` hooks over typed parameters in
/// hook order. Replacements feed the next hook; a failure keeps the running
/// parameters with one notice. Only typed parameters travel here, so raw
/// payload bytes cannot change; the caller clamps the final parameters
/// with [`clamp_params`].
pub async fn dispatch_before_request(
    ext: &str,
    cx: &DispatchCx<'_>,
    hooks: &[Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>],
    event: &BeforeRequest,
    params: RequestParams,
) -> BeforeRequestStep {
    let deadline = cx.deadline();
    let mut current = params;
    let mut notices = Vec::new();
    for hook in hooks {
        let input = BeforeRequest {
            params: current.clone(),
            ..event.clone()
        };
        match settle(hook.call(input, cx.mint(deadline)), cx.cancel, deadline).await {
            Ok(Some(next)) => current = next,
            Ok(None) => {}
            Err(_) => notices.push(before_request_failed_notice(ext)),
        }
    }
    BeforeRequestStep {
        params: current,
        notices,
    }
}
