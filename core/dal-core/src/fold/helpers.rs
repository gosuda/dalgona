use super::{
    AssistantStop, Block, CallId, CompactionReason, Entry, EntryKind, Expect, Family, InferFailure,
    Inference, JournalPart, ModelRoute, Notice, Part, PartialResponse, PendingCall, RawJson,
    Rejection, ResolveError, ResolvedCall, Stop, StreamChannel, StreamEvent, TurnEndStop, TurnId,
    TurnStage, TurnState, UpdateKind, Usage,
};
use crate::model::ReplayCapture;

pub(super) fn invalid(text: &str) -> Rejection {
    Rejection::Invalid {
        reason: text.into(),
    }
}
pub(super) fn wrong_turn(expected: Expect, actual: TurnState) -> Rejection {
    Rejection::WrongTurn { expected, actual }
}
pub(super) fn zero_usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: None,
        cost_usd: None,
    }
}
pub(super) fn assistant_stop_for_end(stop: &TurnEndStop) -> AssistantStop {
    match stop {
        TurnEndStop::Done | TurnEndStop::MaxSteps => AssistantStop::Done,
        TurnEndStop::Length => AssistantStop::Length,
        TurnEndStop::Filter => AssistantStop::Filter,
        TurnEndStop::Cancelled => AssistantStop::Cancelled,
        TurnEndStop::Aborted => AssistantStop::Failed {
            message: "turn aborted".into(),
        },
        TurnEndStop::Failed { message } => AssistantStop::Failed {
            message: message.clone(),
        },
    }
}
pub(super) fn assistant_stop(stop: Stop) -> AssistantStop {
    match stop {
        Stop::EndTurn | Stop::MaxSteps => AssistantStop::Done,
        Stop::Length => AssistantStop::Length,
        Stop::Filter => AssistantStop::Filter,
        Stop::Cancelled => AssistantStop::Cancelled,
        Stop::Failed => AssistantStop::Failed {
            message: "provider ended with a failed stop".into(),
        },
    }
}
pub(super) fn compact_notice(turn: Option<TurnId>, key: &str) -> UpdateKind {
    let (kind, text) = match key {
        "compact.nothing" => ("compact.nothing", "Nothing to compact (session too small)."),
        "compact.already" => ("compact.already", "Already compacted."),
        "compact.none" => (
            "compaction_none",
            "No compactor is registered, so auto-compaction does nothing in this session.",
        ),
        "compact.breaker" => (
            "compaction_off",
            "Auto-compaction is off for this session after 3 failed attempts. Run /compact to try again.",
        ),
        _ => (key, key),
    };
    UpdateKind::Notice(Notice {
        turn,
        kind: kind.into(),
        text: text.into(),
    })
}

pub(super) fn compaction_started_notice(
    turn: Option<TurnId>,
    reason: CompactionReason,
    tokens: Option<u64>,
    window: u64,
) -> UpdateKind {
    let reason = match reason {
        CompactionReason::Threshold => "threshold",
        CompactionReason::Overflow => "overflow",
        CompactionReason::Manual => "manual",
    };
    let text = match (tokens, window) {
        (Some(tokens), window) if window > 0 => {
            let percent = u128::from(tokens) * 100 / u128::from(window);
            format!("Compacting context ({reason}, {percent} percent of the window).")
        }
        _ => format!("Compacting context ({reason}); usage or context window is unavailable."),
    };
    UpdateKind::Notice(Notice {
        turn,
        kind: "compaction_started".into(),
        text: text.into(),
    })
}
pub(super) fn entry_weight(entry: &Entry) -> u64 {
    match &entry.kind {
        EntryKind::User { parts } | EntryKind::ToolResult { parts, .. } => {
            parts.iter().map(journal_part_bytes).sum()
        }
        EntryKind::Assistant { content, .. } => content.iter().map(block_bytes).sum(),
        EntryKind::Reminder { source, text }
            if source.as_ref() == crate::BEFORE_TURN_SOURCE
                || source.as_ref().starts_with("rule:") =>
        {
            text.len() as u64
        }
        EntryKind::Compaction { summary, parts, .. } => {
            if parts.is_empty() {
                summary.as_ref().map_or(0, |text| text.len() as u64)
            } else {
                parts.iter().map(journal_part_bytes).sum()
            }
        }
        _ => 0,
    }
}
pub(super) fn entry_token_weight(entry: &Entry) -> u64 {
    match &entry.kind {
        EntryKind::Assistant { usage, .. } if usage.input_tokens > 0 => usage.input_tokens,
        EntryKind::Compaction {
            parts,
            parts_tokens,
            ..
        } if !parts.is_empty() => *parts_tokens,
        _ => entry_weight(entry) / 4,
    }
}
pub(super) fn journal_part_bytes(part: &JournalPart) -> u64 {
    match part {
        JournalPart::Text { text } => text.len() as u64,
        JournalPart::TextBlob { bytes, .. }
        | JournalPart::ImageBlob { bytes, .. }
        | JournalPart::Blob { bytes, .. } => *bytes,
        JournalPart::Image { base64, .. } => base64.len() as u64,
    }
}
pub(super) fn block_bytes(block: &Block) -> u64 {
    match block {
        Block::Text { text, .. } | Block::Reasoning { text, .. } => text.len() as u64,
        Block::ToolCall { input, .. } => input.as_str().len() as u64,
    }
}
pub(super) fn parts_to_text(parts: &[Part]) -> String {
    let mut out = String::new();
    for part in parts {
        let Part::Text { text } = part else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    out
}
pub(super) fn part_to_journal(part: &Part) -> JournalPart {
    match part {
        Part::Text { text } => JournalPart::Text { text: text.clone() },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } => {
            let blob = blob_id.to_string().into();
            if mime.starts_with("text/") {
                JournalPart::TextBlob {
                    blob,
                    bytes: *bytes,
                }
            } else if mime.starts_with("image/") {
                JournalPart::ImageBlob {
                    mime: mime.clone(),
                    blob,
                    bytes: *bytes,
                }
            } else {
                JournalPart::Blob {
                    mime: mime.clone(),
                    blob,
                    bytes: *bytes,
                }
            }
        }
        Part::Image { mime, bytes } => JournalPart::Image {
            mime: mime.clone(),
            base64: base64(bytes),
        },
    }
}
pub(super) fn duplicate_call_failure(call: &ResolvedCall) -> Box<str> {
    format!(
        "invalid arguments for {}: duplicate call id {}",
        call.name,
        call.call.as_str()
    )
    .into()
}
/// The model-visible result for a call that failed resolution, or `None` when it runs.
pub(super) fn resolution_failure(call: &ResolvedCall) -> Option<Box<str>> {
    match &call.result {
        Ok(_) => None,
        Err(ResolveError::DuplicateCallId) => Some(duplicate_call_failure(call)),
        Err(ResolveError::Unknown) => Some(format!("unknown tool: {}", call.name).into()),
        Err(ResolveError::EvalOnly) => {
            Some(format!("{} is callable only from eval cells", call.name).into())
        }
        Err(ResolveError::InvalidArgs(detail)) => {
            Some(format!("invalid arguments for {}: {detail}", call.name).into())
        }
    }
}
/// The calls a turn stage tracks as pending.
fn staged_pending(stage: &TurnStage) -> &[PendingCall] {
    match stage {
        TurnStage::Dispatching { pending } | TurnStage::Resolving { pending } => pending,
        TurnStage::Streaming { .. } | TurnStage::Boundary | TurnStage::Compacting { .. } => &[],
    }
}

/// The tool calls of a journaled partial response that `staged` does not
/// track, each call identity once, in content order.
fn untracked_partial_calls<'a>(
    staged: &'a [PendingCall],
    partial: Option<&'a PartialResponse>,
) -> impl Iterator<Item = (&'a CallId, &'a str)> {
    let content: &[Block] = match partial {
        Some(partial) => partial.content.as_slice(),
        None => &[],
    };
    content
        .iter()
        .enumerate()
        .filter_map(move |(index, block)| {
            let Block::ToolCall { id, name, .. } = block else {
                return None;
            };
            let tracked = staged.iter().any(|item| item.call == *id)
                || content[..index].iter().any(
                    |prior| matches!(prior, Block::ToolCall { id: other, .. } if other == id),
                );
            (!tracked).then_some((id, &**name))
        })
}

/// Calls that still need a result when a turn ends: the stage's pending calls, then
/// tool calls from a journaled partial response that no stage tracked.
///
/// The list is owned because the turn ending rewrites the phase that holds the stage.
/// [`unsettled_call_count`] counts the same list without building it.
pub(super) fn unsettled_calls(
    stage: &TurnStage,
    partial: Option<&PartialResponse>,
) -> Vec<PendingCall> {
    let staged = staged_pending(stage);
    let mut pending = staged.to_vec();
    pending.extend(
        untracked_partial_calls(staged, partial).map(|(call, name)| PendingCall {
            call: call.clone(),
            name: name.into(),
            started: false,
            promotes: None,
        }),
    );
    pending
}

/// The length of [`unsettled_calls`], without copying a call.
pub(super) fn unsettled_call_count(stage: &TurnStage, partial: Option<&PartialResponse>) -> usize {
    let staged = staged_pending(stage);
    staged.len() + untracked_partial_calls(staged, partial).count()
}
/// The call and extension a guarding hook result names.
pub(super) struct HookTarget {
    pub(super) call: Option<CallId>,
    pub(super) extension: Option<Box<str>>,
}

/// The final report of one provider request, as carried by [`Event::StreamEnded`].
pub(super) struct StreamEnd {
    pub(super) model: ModelRoute,
    pub(super) family: Family,
    pub(super) result: Result<Inference, InferFailure>,
    pub(super) partial: Option<PartialResponse>,
}

/// A provider request that returned a complete inference.
pub(super) struct CompletedResponse {
    pub(super) model: ModelRoute,
    pub(super) family: Family,
    pub(super) inference: Inference,
}

/// One completed provider response split into journal blocks and its stream facts.
pub(super) struct InferredResponse {
    pub(super) blocks: Vec<Block>,
    pub(super) calls: Vec<(CallId, Box<str>)>,
    pub(super) usage: Option<Usage>,
    pub(super) stop: Option<Stop>,
    /// The response's exact provider replay capture, when its stream carried
    /// one. A tool-call response is not a message replay: the policy drops a
    /// `MessageReplay` event there under the existing late-event rule, so
    /// `stop` still decides the call flow.
    pub(super) replay: Option<ReplayCapture>,
}

pub(super) fn blocks_from_inference(inference: Inference) -> Result<InferredResponse, Rejection> {
    let mut blocks = Vec::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut stop = None;
    let mut replay = None;
    // The event grammar puts the capture after the tool calls and before the
    // usage; a capture arriving outside that window is a late event, dropped
    // in release and flagged in debug, like every other late stream event.
    let mut replay_closed = false;
    let mut current_reasoning = None;
    // Adjacent text deltas are one run of prose: a block per token would split
    // a reply into one transcript row per delta.
    let mut text_run = String::new();
    for event in inference.events {
        match event {
            StreamEvent::Delta {
                channel: StreamChannel::Text,
                text,
            } => {
                current_reasoning = None;
                text_run.push_str(&text);
            }
            StreamEvent::Delta {
                channel: StreamChannel::Thinking,
                text,
            } => {
                flush_text_run(&mut blocks, &mut text_run);
                push_thinking(&mut blocks, &mut current_reasoning, text);
            }
            StreamEvent::ThinkingReplay { payload } => {
                flush_text_run(&mut blocks, &mut text_run);
                attach_thinking_replay(&mut blocks, &mut current_reasoning, payload);
            }
            StreamEvent::Delta {
                channel: StreamChannel::ToolArgs { .. },
                ..
            } => {}
            StreamEvent::ToolCall { call, name, args } => {
                flush_text_run(&mut blocks, &mut text_run);
                current_reasoning = None;
                calls.push((call.clone(), name.clone()));
                blocks.push(Block::ToolCall {
                    id: call,
                    name,
                    input: args,
                    replay: None,
                });
            }
            StreamEvent::Usage(measurement) => {
                // A capture must not follow the usage: anything arriving now
                // is outside the grammar's window and is dropped as a late
                // event rather than attached to the response.
                replay_closed = true;
                usage = Some(measurement);
            }
            StreamEvent::Stop(reason) => stop = Some(reason),
            StreamEvent::MessageReplay { capture } => {
                // One capture per response, inside the grammar's window: not
                // after the usage. A duplicate, or a capture outside the
                // window, is a late event under the existing policy - flagged
                // in debug, dropped in release, and never allowed to
                // overwrite a capture already accepted. Whether the response
                // may carry a capture at all is decided once the whole
                // response is known, after the loop.
                if replay_closed {
                    debug_assert!(
                        false,
                        "message replay event arrived after its response's usage; add an explicit policy"
                    );
                } else if replay.is_none() {
                    replay = Some(capture);
                } else {
                    debug_assert!(
                        false,
                        "duplicate message replay event; add an explicit policy"
                    );
                }
            }
            StreamEvent::Compaction { .. } => {
                return Err(invalid(
                    "native compaction outcome appeared in a turn stream",
                ));
            }
        }
    }
    flush_text_run(&mut blocks, &mut text_run);
    // A response that called tools is not a message replay: there is no
    // closing entry for the capture to ride on. Scripted providers emit the
    // capture alongside tool calls legitimately, so this is a silent drop
    // under the same policy that already discarded its streamed state.
    if !calls.is_empty() {
        replay = None;
    }
    Ok(InferredResponse {
        blocks,
        calls,
        usage,
        stop,
        replay,
    })
}

fn push_thinking(blocks: &mut Vec<Block>, current: &mut Option<usize>, text: Box<str>) {
    if let Some(Block::Reasoning { text: previous, .. }) =
        (*current).and_then(|index| blocks.get_mut(index))
    {
        let mut combined = std::mem::take(previous).into_string();
        combined.push_str(&text);
        *previous = combined.into_boxed_str();
    } else if current.is_none() {
        blocks.push(Block::Reasoning {
            text,
            replay: RawJson::null(),
        });
        *current = Some(blocks.len() - 1);
    }
}

fn attach_thinking_replay(blocks: &mut Vec<Block>, current: &mut Option<usize>, payload: RawJson) {
    if let Some(Block::Reasoning { replay, .. }) =
        (*current).and_then(|index| blocks.get_mut(index))
    {
        *replay = payload;
    } else if current.is_none() {
        blocks.push(Block::Reasoning {
            text: String::new().into(),
            replay: payload,
        });
        *current = Some(blocks.len() - 1);
    }
}

/// Closes the pending run of text deltas as one block.
fn flush_text_run(blocks: &mut Vec<Block>, text_run: &mut String) {
    if !text_run.is_empty() {
        blocks.push(Block::Text {
            text: std::mem::take(text_run).into_boxed_str(),
            replay: None,
        });
    }
}
pub(super) fn base64(bytes: &[u8]) -> Box<str> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(c & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out.into_boxed_str()
}
pub(super) fn truncated_args(name: &str) -> Box<str> {
    format!("Tool call \"{name}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.").into()
}
