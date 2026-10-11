//! Compaction request bodies, response parsers, and the Codex event scan.
//!
//! Histories are constructed only from complete provider responses, so a
//! cancelled or failed attempt can never publish a partial one.

use std::{
    borrow::Cow,
    sync::atomic::{AtomicBool, Ordering},
};

use dal_core::{Family, RawJson};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{
    AttemptResult, CompactedHistory,
    support::{invalid, invalid_json, missing, protocol, redact_stream_error, source_offset},
};
use crate::{error::ProviderError, family::responses, lifecycle::AttemptFailure, sse::SseEvent};

pub(crate) fn compact_responses_body(
    body: &[u8],
    expected_model: &str,
) -> Result<Vec<u8>, ProviderError> {
    let request = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    let mut model = None;
    let mut instructions = None;
    let mut input = None;
    for member in sonic_rs::to_object_iter(request) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        match name.as_ref() {
            "model" => model = Some(value.as_raw_cow()),
            "instructions" => instructions = Some(value.as_raw_cow()),
            "input" => input = Some(value.as_raw_cow()),
            _ => {}
        }
    }
    let model = model.ok_or_else(|| invalid("Responses body has no model"))?;
    let instructions = instructions.ok_or_else(|| invalid("Responses body has no instructions"))?;
    let input = input.ok_or_else(|| invalid("Responses body has no input"))?;
    let actual_model: String =
        sonic_rs::from_str(model.as_ref()).map_err(|error| invalid_json("model", error))?;
    if actual_model != expected_model {
        return Err(ProviderError::InvalidRequest {
            message: String::from("Responses compaction body model does not match its request"),
        });
    }
    let mut compact = Vec::with_capacity(model.len() + instructions.len() + input.len() + 38);
    compact.extend_from_slice(b"{\"model\":");
    compact.extend_from_slice(model.as_bytes());
    compact.extend_from_slice(b",\"instructions\":");
    compact.extend_from_slice(instructions.as_bytes());
    compact.extend_from_slice(b",\"input\":");
    compact.extend_from_slice(input.as_bytes());
    compact.push(b'}');
    Ok(compact)
}

pub(crate) fn codex_compaction_body(body: &[u8]) -> Result<(Vec<RawJson>, Vec<u8>), ProviderError> {
    let request = std::str::from_utf8(body).map_err(|error| invalid_json("request body", error))?;
    let mut input = None;
    for member in sonic_rs::to_object_iter(request) {
        let (name, value) = member.map_err(|error| invalid_json("request body", error))?;
        if name.as_ref() == "input" {
            input = Some(value.as_raw_cow());
            break;
        }
    }
    let input = input.ok_or_else(|| invalid("Responses body has no input"))?;
    let inputs = sonic_rs::to_array_iter(input.as_ref())
        .map(|item| {
            item.map(|item| item.as_raw_cow())
                .map_err(|error| invalid_json("input array", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let retained = retain_user_messages(&inputs)?;
    let request = append_input_item(body, input.as_ref(), &inputs)?;
    Ok((retained, request))
}

fn append_input_item(
    body: &[u8],
    input: &str,
    inputs: &[Cow<'_, str>],
) -> Result<Vec<u8>, ProviderError> {
    let input_start = source_offset(body, input)?;
    let insert = match inputs.last() {
        Some(last) => source_offset(body, last.as_ref())?
            .checked_add(last.len())
            .ok_or_else(|| invalid("Responses input offset overflowed"))?,
        None => input_start + 1,
    };
    let mut result = Vec::with_capacity(body.len() + super::COMPACTION_TRIGGER.len() + 1);
    result.extend_from_slice(&body[..insert]);
    if !inputs.is_empty() {
        result.push(b',');
    }
    result.extend_from_slice(super::COMPACTION_TRIGGER);
    result.extend_from_slice(&body[insert..]);
    Ok(result)
}

pub(crate) fn retain_user_messages(inputs: &[Cow<'_, str>]) -> Result<Vec<RawJson>, ProviderError> {
    let mut retained = Vec::new();
    let mut bytes = 0_usize;
    for item in inputs.iter().rev() {
        let item = item.as_ref();
        let role: InputRole<'_> = sonic_rs::from_str(item)
            .map_err(|error| invalid(format!("Responses input item is invalid: {error}")))?;
        if role.role != Some("user") {
            continue;
        }
        if item.len() > super::CODEX_RETAINED_USER_BYTE_LIMIT - bytes {
            break;
        }
        bytes += item.len();
        retained.push(
            RawJson::parse(item)
                .map_err(|error| invalid(format!("invalid retained user message: {error}")))?,
        );
    }
    retained.reverse();
    Ok(retained)
}

#[derive(Deserialize)]
struct InputRole<'a> {
    #[serde(borrow)]
    role: Option<&'a str>,
}
pub(crate) fn parse_responses_output(
    bytes: &[u8],
    model: &str,
) -> Result<CompactedHistory, ProviderError> {
    let response: ResponsesCompactReply = sonic_rs::from_slice(bytes).map_err(|error| {
        protocol(
            Family::Responses,
            format!("malformed compact response: {error}"),
        )
    })?;
    Ok(CompactedHistory {
        family: Family::Responses,
        model: model.into(),
        items: response.output,
    })
}

#[derive(Deserialize)]
struct ResponsesCompactReply {
    output: Vec<RawJson>,
}

pub(crate) fn parse_anthropic_block(
    bytes: &[u8],
    model: &str,
) -> Result<CompactedHistory, ProviderError> {
    let reply: AnthropicCompactReply = sonic_rs::from_slice(bytes).map_err(|error| {
        protocol(
            Family::Anthropic,
            format!("malformed compact response: {error}"),
        )
    })?;
    if reply.stop_reason.as_deref() != Some("compaction") {
        return Err(missing(Family::Anthropic, "block"));
    }
    let Some(block) = reply.content.into_iter().next() else {
        return Err(missing(Family::Anthropic, "block"));
    };
    let kind: BlockKind = block.decode_as().map_err(|error| {
        protocol(
            Family::Anthropic,
            format!("malformed compaction block: {error}"),
        )
    })?;
    if kind.kind != "compaction" {
        return Err(missing(Family::Anthropic, "block"));
    }
    Ok(CompactedHistory {
        family: Family::Anthropic,
        model: model.into(),
        items: vec![block],
    })
}

#[derive(Deserialize)]
struct AnthropicCompactReply {
    stop_reason: Option<String>,
    #[serde(default)]
    content: Vec<RawJson>,
}

#[derive(Deserialize)]
struct BlockKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct CompactEventHead {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct OutputItemDone {
    item: RawJson,
}

#[derive(Deserialize)]
struct OutputItemKind {
    #[serde(rename = "type")]
    kind: String,
}

pub(crate) async fn decode_codex_events<S>(
    events: S,
    model: Box<str>,
    retained_users: Vec<RawJson>,
    read_failed: &AtomicBool,
    token: &str,
    cancel: &CancellationToken,
) -> AttemptResult
where
    S: Stream<Item = Result<SseEvent, ProviderError>>,
{
    let mut events = Box::pin(events);
    let mut decoder = responses::Decoder::new(Family::Codex, model.clone());
    let mut compaction = None;
    let mut ignored = Vec::new();
    loop {
        let next = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(None),
            next = events.next() => next,
        };
        let Some(event) = next else {
            if read_failed.load(Ordering::Acquire) {
                return Err(AttemptFailure::Provider(ProviderError::Transport {
                    family: Family::Codex,
                    reason: String::from("Codex response body read failed"),
                }));
            }
            return Err(AttemptFailure::Provider(ProviderError::StreamCut));
        };
        let event = event.map_err(AttemptFailure::Provider)?;
        if event.data == "[DONE]" {
            continue;
        }
        ignored.clear();
        let terminal = match decoder.feed(&event.data, &mut ignored) {
            Ok(terminal) => terminal,
            Err(error) => {
                return Err(AttemptFailure::Provider(redact_stream_error(error, token)));
            }
        };
        let head: CompactEventHead = sonic_rs::from_str(&event.data).map_err(|error| {
            AttemptFailure::Provider(protocol(Family::Codex, format!("malformed event: {error}")))
        })?;
        if head.kind == "response.output_item.done" {
            let done: OutputItemDone = sonic_rs::from_str(&event.data).map_err(|error| {
                AttemptFailure::Provider(protocol(
                    Family::Codex,
                    format!("malformed output item: {error}"),
                ))
            })?;
            let item: OutputItemKind = done.item.decode_as().map_err(|error| {
                AttemptFailure::Provider(protocol(
                    Family::Codex,
                    format!("malformed output item: {error}"),
                ))
            })?;
            if matches!(
                item.kind.as_str(),
                "compaction" | "compaction_summary" | "context_compaction"
            ) && compaction.is_none()
            {
                compaction = Some(done.item);
            }
        }
        if terminal {
            if head.kind != "response.completed" {
                if compaction.is_none() {
                    return Err(AttemptFailure::Provider(missing(Family::Codex, "item")));
                }
                return Err(AttemptFailure::Provider(protocol(
                    Family::Codex,
                    "compaction response did not complete",
                )));
            }
            let Some(compaction) = compaction else {
                return Err(AttemptFailure::Provider(missing(Family::Codex, "item")));
            };
            let mut items = retained_users;
            items.push(compaction);
            return Ok(Some(CompactedHistory {
                family: Family::Codex,
                model,
                items,
            }));
        }
    }
}
