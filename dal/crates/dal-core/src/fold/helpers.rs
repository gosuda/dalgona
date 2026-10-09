use super::{
    AssistantStop, Block, CallId, CompactionReason, Entry, EntryKind, Expect, Family, InferFailure,
    Inference, JobKind, JournalPart, ModelRoute, Notice, Part, PartialResponse, PendingCall,
    RawJson, Rejection, ResolveError, ResolvedCall, Stop, StreamChannel, StreamEvent, TurnEndStop,
    TurnId, TurnStage, TurnState, UpdateKind, Usage,
};

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
        EntryKind::Reminder { text, .. } => text.len() as u64,
        EntryKind::Compaction {
            summary,
            replay,
            parts,
            ..
        } => {
            summary.as_ref().map_or(0, |text| text.len() as u64)
                + replay.as_ref().map_or(0, |json| json.as_str().len() as u64)
                + parts.iter().map(journal_part_bytes).sum::<u64>()
        }
        _ => 0,
    }
}
pub(super) fn entry_token_weight(entry: &Entry) -> u64 {
    match &entry.kind {
        EntryKind::Assistant { usage, .. } => usage.input_tokens,
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
        Block::Text { text } | Block::Reasoning { text, .. } => text.len() as u64,
        Block::ToolCall { input, .. } => input.as_str().len() as u64,
    }
}
pub(super) fn parse_job_kind(value: Option<&str>) -> Option<JobKind> {
    match value? {
        "exec" => Some(JobKind::Exec),
        "child" => Some(JobKind::Child),
        "compaction" => Some(JobKind::Compaction),
        _ => None,
    }
}
pub(super) fn parts_to_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_ref()),
            Part::Image { .. } | Part::Blob { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub(super) fn part_to_journal(part: &Part) -> JournalPart {
    match part {
        Part::Text { text } => JournalPart::Text { text: text.clone() },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } if mime.starts_with("text/") => JournalPart::TextBlob {
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } if mime.starts_with("image/") => JournalPart::ImageBlob {
            mime: mime.clone(),
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Blob {
            blob_id,
            mime,
            bytes,
        } => JournalPart::Blob {
            mime: mime.clone(),
            blob: hex(blob_id.as_bytes()),
            bytes: *bytes,
        },
        Part::Image { mime, bytes } => JournalPart::Image {
            mime: mime.clone(),
            base64: base64(bytes),
        },
    }
}
/// The model-visible result for a call that failed resolution, or `None` when it runs.
pub(super) fn resolution_failure(call: &ResolvedCall, duplicate: bool) -> Option<Box<str>> {
    let text = if duplicate || matches!(call.result, Err(ResolveError::DuplicateCallId)) {
        format!(
            "invalid arguments for {}: duplicate call id {}",
            call.name,
            call.call.as_str()
        )
    } else {
        match &call.result {
            Ok(_) | Err(ResolveError::DuplicateCallId) => return None,
            Err(ResolveError::Unknown) => format!("unknown tool: {}", call.name),
            Err(ResolveError::EvalOnly) => {
                format!("{} is callable only from eval cells", call.name)
            }
            Err(ResolveError::InvalidArgs(detail)) => {
                format!("invalid arguments for {}: {detail}", call.name)
            }
        }
    };
    Some(text.into())
}
/// Calls that still need a result when a turn ends: the stage's pending calls, then
/// tool calls from a journaled partial response that no stage tracked.
pub(super) fn unsettled_calls(
    stage: TurnStage,
    partial: Option<&PartialResponse>,
) -> Vec<PendingCall> {
    let mut pending = match stage {
        TurnStage::Dispatching { pending } | TurnStage::Resolving { pending } => pending,
        TurnStage::Streaming { .. } | TurnStage::Boundary | TurnStage::Compacting { .. } => {
            Vec::new()
        }
    };
    let partial_calls = partial.into_iter().flat_map(|partial| &partial.content);
    for block in partial_calls {
        if let Block::ToolCall { id, name, .. } = block
            && !pending.iter().any(|item| item.call == *id)
        {
            pending.push(PendingCall {
                call: id.clone(),
                name: name.clone(),
                started: false,
                promotes: None,
            });
        }
    }
    pending
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
}

pub(super) fn blocks_from_inference(inference: Inference) -> Result<InferredResponse, Rejection> {
    let mut blocks = Vec::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut stop = None;
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
                if let Some(index) = current_reasoning {
                    if let Some(Block::Reasoning { text: previous, .. }) = blocks.get_mut(index) {
                        let mut combined = previous.to_string();
                        combined.push_str(&text);
                        *previous = combined.into();
                    }
                } else {
                    blocks.push(Block::Reasoning {
                        text,
                        replay: RawJson::null(),
                    });
                    current_reasoning = Some(blocks.len() - 1);
                }
            }
            StreamEvent::ThinkingReplay { payload } => {
                flush_text_run(&mut blocks, &mut text_run);
                if let Some(index) = current_reasoning {
                    if let Some(Block::Reasoning { replay, .. }) = blocks.get_mut(index) {
                        *replay = payload;
                    }
                } else {
                    blocks.push(Block::Reasoning {
                        text: String::new().into(),
                        replay: payload,
                    });
                    current_reasoning = Some(blocks.len() - 1);
                }
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
                });
            }
            StreamEvent::Usage(measurement) => usage = Some(measurement),
            StreamEvent::Stop(reason) => stop = Some(reason),
            StreamEvent::Compaction { .. } => {
                return Err(invalid(
                    "native compaction outcome appeared in a turn stream",
                ));
            }
        }
    }
    flush_text_run(&mut blocks, &mut text_run);
    Ok(InferredResponse {
        blocks,
        calls,
        usage,
        stop,
    })
}

/// Closes the pending run of text deltas as one block.
fn flush_text_run(blocks: &mut Vec<Block>, text_run: &mut String) {
    if !text_run.is_empty() {
        blocks.push(Block::Text {
            text: std::mem::take(text_run).into_boxed_str(),
        });
    }
}
pub(super) fn hex(bytes: &[u8]) -> Box<str> {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out.into()
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
