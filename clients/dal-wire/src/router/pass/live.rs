//! Live relay: provider events to turn events, pumped into one SSE sink.

use dal_core::Family;
use dal_provider::{ProviderError, StopReason, StreamEvent, ToolArgs};
use tokio_util::sync::CancellationToken;

use super::super::harness::{HarnessEvent, HarnessStop, UsageSum};
use super::super::sink::{EventSink, SseEncoder, StreamSink};
use super::super::stream::TurnIds;
use super::{Relay, provider_fail};

/// One started tool call with its undecoded argument tail and decoded text.
struct PendingCall {
    id: String,
    tail: Vec<u8>,
    text: String,
}

/// Maps provider stream events to turn events for one route/wire pair.
pub(crate) struct RelayMapper {
    route: Option<Family>,
    wire: Family,
    calls: Vec<PendingCall>,
}

impl RelayMapper {
    /// Creates a mapper for a route family and the client wire family.
    pub(crate) const fn new(route: Option<Family>, wire: Family) -> Self {
        Self {
            route,
            wire,
            calls: Vec::new(),
        }
    }

    /// Maps one provider event, appending its turn events to `out`.
    pub(crate) fn map(&mut self, event: StreamEvent, out: &mut Vec<HarnessEvent>) {
        match event {
            StreamEvent::TextDelta { text } => out.push(HarnessEvent::Text(text)),
            StreamEvent::ReasoningDelta { text } => out.push(HarnessEvent::Reasoning(text)),
            StreamEvent::ToolCallStarted { id, name } => {
                self.calls.push(PendingCall {
                    id: id.clone(),
                    tail: Vec::new(),
                    text: String::new(),
                });
                out.push(HarnessEvent::ToolCallStarted { id, name });
            }
            StreamEvent::ToolArgsDelta { id, fragment } => self.args(id, &fragment, out),
            StreamEvent::Replay { payload } => {
                if self.replay_preserved()
                    && let Ok(item) = sonic_rs::from_str(payload.item.as_str())
                {
                    out.push(HarnessEvent::Replay(item));
                }
            }
            StreamEvent::ToolCallsDone { calls } => {
                for call in calls {
                    self.done(call, out);
                }
            }
            StreamEvent::Usage { usage } => out.push(HarnessEvent::Usage(UsageSum {
                input: usage.input_tokens,
                output: usage.output_tokens,
                cache_read: usage.cached_input_tokens,
                cache_write: usage.cache_write_tokens,
                cost: usage.cost_usd,
                ..UsageSum::default()
            })),
            StreamEvent::Stop { reason } => out.push(HarnessEvent::Stop(stop_of(reason))),
            _ => {}
        }
    }

    /// Decodes the complete UTF-8 prefix of one argument fragment.
    fn args(&mut self, id: String, fragment: &[u8], out: &mut Vec<HarnessEvent>) {
        let Some(call) = self.calls.iter_mut().find(|call| call.id == id) else {
            return;
        };
        call.tail.extend_from_slice(fragment);
        let valid = match std::str::from_utf8(&call.tail) {
            Ok(text) => text.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => call.tail.len(),
        };
        let rest = call.tail.split_off(valid);
        let text = String::from_utf8_lossy(&call.tail).into_owned();
        call.tail = rest;
        if text.is_empty() {
            return;
        }
        call.text.push_str(&text);
        out.push(HarnessEvent::ToolArgs { id, fragment: text });
    }

    /// Completes one tool call, starting it first when the provider never did.
    fn done(&mut self, call: dal_provider::ToolCall, out: &mut Vec<HarnessEvent>) {
        let parsed = match &call.args {
            ToolArgs::Parsed(raw) => Some(raw.as_str().to_owned()),
            ToolArgs::Invalid { .. } | ToolArgs::Truncated => None,
        };
        let streamed = self
            .calls
            .iter()
            .position(|pending| pending.id == call.id)
            .map(|index| self.calls.swap_remove(index).text);
        let args = match (parsed, streamed) {
            (Some(args), Some(text)) => {
                if text.is_empty() && !args.is_empty() {
                    out.push(HarnessEvent::ToolArgs {
                        id: call.id.clone(),
                        fragment: args.clone(),
                    });
                }
                args
            }
            (None, Some(text)) => text,
            (parsed, None) => {
                let args = parsed.unwrap_or_default();
                out.push(HarnessEvent::ToolCallStarted {
                    id: call.id.clone(),
                    name: call.name.clone(),
                });
                if !args.is_empty() {
                    out.push(HarnessEvent::ToolArgs {
                        id: call.id.clone(),
                        fragment: args.clone(),
                    });
                }
                args
            }
        };
        out.push(HarnessEvent::ToolCallDone {
            id: call.id,
            name: call.name,
            args,
        });
    }

    /// Returns true when thinking replay survives the route/wire pair.
    fn replay_preserved(&self) -> bool {
        matches!(
            (self.route, self.wire),
            (Some(Family::Anthropic), Family::Anthropic)
                | (Some(Family::Responses), Family::Responses)
        )
    }
}

/// Maps a provider stop reason to its turn stop.
fn stop_of(reason: StopReason) -> HarnessStop {
    match reason {
        StopReason::EndTurn | StopReason::Paused => HarnessStop::EndTurn,
        StopReason::ToolUse => HarnessStop::ToolUse,
        StopReason::MaxTokens => HarnessStop::MaxTokens,
        StopReason::Refusal => HarnessStop::Refusal,
        StopReason::Other(message) => {
            HarnessStop::Failed(format!("upstream provider failed: {message}"))
        }
    }
}

/// Maps a provider failure after the response started to its stream stop.
fn failure(error: ProviderError) -> HarnessEvent {
    HarnessEvent::Stop(HarnessStop::Failed(provider_fail(error).message))
}

impl Relay {
    /// Streams the relay into `sink` as provider events arrive.
    ///
    /// A gone client or a stopping listener drops the provider stream, which
    /// cancels its transport; a stopping listener also sends the family's
    /// cancellation event when the channel has room.
    pub(crate) async fn pump<E: SseEncoder>(
        self,
        ids: TurnIds,
        shutdown: &CancellationToken,
        sink: &mut StreamSink<E>,
    ) {
        let Self {
            mut stream,
            first,
            mut mapper,
        } = self;
        let mut events = vec![HarnessEvent::Started(ids)];
        mapper.map(first, &mut events);
        loop {
            for event in events.drain(..) {
                let terminal = matches!(event, HarnessEvent::Stop(_));
                let live = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => {
                        sink.emit_now(&HarnessEvent::Stop(HarnessStop::Cancelled));
                        return;
                    }
                    live = sink.emit(event) => live,
                };
                if !live || terminal {
                    return;
                }
            }
            let item = tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    sink.emit_now(&HarnessEvent::Stop(HarnessStop::Cancelled));
                    return;
                }
                () = sink.closed() => return,
                item = stream.next() => item,
            };
            match item {
                Some(Ok(event)) => mapper.map(event, &mut events),
                Some(Err(error)) => events.push(failure(error)),
                None => return,
            }
        }
    }
}
