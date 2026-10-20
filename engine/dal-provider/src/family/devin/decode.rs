//! Devin response decoding: frames into neutral stream events.
//!
//! [`ChatDecoder`] consumes complete Connect frames and emits the provider
//! [`StreamEvent`] grammar: text, thinking, and tool deltas in arrival order,
//! then one `ToolCallsDone`, at most one thinking `Replay`, at most one
//! message `MessageReplay`, one `Usage`, and one `Stop` — or one error
//! instead. [`decode_chunks`] drives a decoder over a raw byte stream; the
//! join feeds it the chat response body and wraps the result in an
//! `EventStream`.
//!
//! Terminal semantics, mirroring the pinned reference with one deliberate
//! tightening: an end-of-stream trailer with an error ends the turn with the
//! mapped failure; a clean end-of-stream frame finishes the turn; end of
//! input finishes the turn when a non-zero stop reason arrived, and is a
//! [`ProviderError::StreamCut`] otherwise — including a torn frame. The
//! pinned client tolerates a cut as success; dal cannot, because truncated
//! tool arguments must never run. `MAX_TOKENS` always wins over `ToolUse` and
//! truncates every call, so a length-cut turn never executes cut-off JSON.
//!
//! Thinking signatures replay through one `Replay` item bound to this family
//! and model; the native response id replays through one `MessageReplay`
//! item on turns without tool calls, so the next turn reuses the server's
//! own id. Usage follows the Anthropic normalization: input counts every
//! prompt token, cache reads and writes included. Credit fields have no USD
//! meaning and are dropped; the join owns account-level metering.

use std::collections::VecDeque;

use dal_core::{Family, RawJson, ReplayCapture, Usage};
use futures::{Stream, StreamExt, stream};
use prost::Message;

use super::connect::{
    COMPRESSED_FLAG, END_STREAM_FLAG, FrameDecoder, TrailerError, gunzip_capped, parse_trailer,
};
use super::proto::{
    ChatToolCall, ClientModelConfig, GetChatMessageResponse, GetCliModelConfigsResponse,
    ModelUsageStats, model_dimension_kind, stop_reason,
};
use super::terminal::Terminal;
use crate::error::ProviderError;
use crate::retry::{RequestState, RetryDecision, classify};
use crate::stream::{ReplayPayload, StopReason, StreamEvent, ToolArgs, ToolCall};
use crate::stream_budget::StreamBudget;

/// The HTTP status of the stream response that carries an in-stream error.
const IN_STREAM_STATUS: u16 = 200;

/// Running token counters; the per-counter maximum wins.
///
/// Usage messages carry cumulative snapshots, not deltas, and a message may
/// omit counters the turn already reported (proto3 decodes the absent field
/// as zero). Taking the maximum keeps the latest cumulative total without
/// double-counting, and a sparse all-zero message can never zero an earlier
/// total the way a whole-message overwrite would.
#[derive(Debug, Default)]
struct UsageState {
    /// Prompt tokens.
    input: Option<u64>,
    /// Completion tokens.
    output: Option<u64>,
    /// Cached input tokens.
    cache_read: Option<u64>,
    /// Cache-write tokens.
    cache_write: Option<u64>,
}

impl UsageState {
    /// Folds one usage snapshot into the running counters.
    ///
    /// Each counter keeps its maximum: snapshots are cumulative totals, so
    /// summing would double-count, and a sparse message reporting zero for a
    /// counter it omits must not regress a total an earlier message set.
    fn merge(&mut self, wire: &ModelUsageStats) {
        self.input = Some(fold_max(self.input, wire.input_tokens));
        self.output = Some(fold_max(self.output, wire.output_tokens));
        self.cache_read = Some(fold_max(self.cache_read, wire.cache_read_tokens));
        self.cache_write = Some(fold_max(self.cache_write, wire.cache_write_tokens));
    }

    /// The core counters: input counts every prompt token, cache reads and
    /// writes included; output is reported as-is.
    ///
    /// The wire splits prompt tokens the Anthropic way — `input_tokens`
    /// beside dedicated cache-read and cache-write counters — so the split
    /// is additive here, mirroring the pinned Anthropic normalization, not a
    /// double count of one inclusive total.
    fn usage(&self) -> Usage {
        let cache_read = self.cache_read.unwrap_or(0);
        let cache_write = self.cache_write.unwrap_or(0);
        Usage {
            input_tokens: self
                .input
                .unwrap_or(0)
                .saturating_add(cache_read)
                .saturating_add(cache_write),
            cached_input_tokens: cache_read,
            output_tokens: self.output.unwrap_or(0),
            reasoning_tokens: None,
            cache_write_tokens: cache_write,
            cost_usd: None,
            premium_requests: None,
        }
    }
}

/// Folds one snapshot counter into its running maximum.
///
/// Snapshots are cumulative and sparse: a message reports zero for a counter
/// it omits, so the fold keeps the larger side instead of overwriting.
#[must_use]
fn fold_max(current: Option<u64>, seen: u64) -> u64 {
    current.unwrap_or(0).max(seen)
}

/// One open tool call: id and name plus the raw argument bytes so far.
#[derive(Debug)]
struct OpenCall {
    /// Provider call id.
    id: String,
    /// Tool name, updated by later deltas.
    name: String,
    /// Concatenated argument bytes.
    args: Vec<u8>,
}

/// The incremental decoder of one Devin chat stream.
///
/// Feed it complete frames in wire order with [`ChatDecoder::feed_frame`],
/// then call [`ChatDecoder::finish_eof`] at end of input. It emits at most
/// one terminal group and ignores everything after it.
#[derive(Debug)]
pub(crate) struct ChatDecoder {
    /// Provider id for retry classification.
    provider: Box<str>,
    /// Model id bound to replay items.
    model: Box<str>,
    /// The exactly-one-terminal latch and delivered counter.
    terminal: Terminal,
    /// Stream resource limits.
    budget: StreamBudget,
    /// A thinking run is open.
    thinking_open: bool,
    /// Accumulated thinking text of the open run.
    thinking_text: String,
    /// Latest signature of the open run.
    signature: Option<String>,
    /// Open tool calls in first-seen order.
    calls: Vec<OpenCall>,
    /// Id continued by id-less deltas.
    active_id: Option<String>,
    /// Latest non-zero stop discriminant.
    stop: Option<i32>,
    /// Running usage counters.
    usage: UsageState,
    /// First response id, replayed on tool-free turns.
    message_id: Option<String>,
}

impl ChatDecoder {
    /// A decoder for one response of `model` from `provider`.
    #[must_use]
    pub(crate) fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            terminal: Terminal::new(),
            budget: StreamBudget::default(),
            thinking_open: false,
            thinking_text: String::new(),
            signature: None,
            calls: Vec::new(),
            active_id: None,
            stop: None,
            usage: UsageState::default(),
            message_id: None,
        }
    }

    /// Whether the turn already ended; late input must be ignored.
    #[must_use]
    pub(crate) fn is_finished(&self) -> bool {
        self.terminal.is_finished()
    }

    /// Marks the turn failed with `error` and returns it.
    pub(crate) fn fail(&mut self, error: ProviderError) -> ProviderError {
        self.terminal.finish_err();
        error
    }

    /// Maps a framing failure into a Devin protocol error.
    pub(crate) fn fail_connect(&mut self, error: &super::connect::ConnectError) -> ProviderError {
        self.fail(ProviderError::Protocol {
            family: Family::Devin,
            detail: error.to_string(),
        })
    }

    /// Feeds one complete frame; returns true when the stream is finished.
    ///
    /// End-of-stream frames finish the turn (terminal group on a clean
    /// trailer, the mapped failure on an error trailer). Data frames apply
    /// one response message. Anything after the terminal is ignored.
    pub(crate) fn feed_frame(
        &mut self,
        flags: u8,
        payload: &[u8],
        out: &mut Vec<StreamEvent>,
    ) -> Result<bool, ProviderError> {
        if self.terminal.is_finished() {
            return Ok(true);
        }
        if let Err(error) = self.budget.admit_event(payload.len()) {
            return Err(self.fail(error));
        }
        if flags & END_STREAM_FLAG != 0 {
            let bytes = self.inflated(payload, flags)?;
            return self.finish_trailer(&bytes, out);
        }
        let bytes = self.inflated(payload, flags)?;
        let message = GetChatMessageResponse::decode(bytes.as_slice()).map_err(|error| {
            self.fail(ProviderError::Protocol {
                family: Family::Devin,
                detail: format!("devin chat message did not decode: {error}"),
            })
        })?;
        let before = out.len();
        self.apply(&message, out)?;
        self.terminal.note_events(out.len().saturating_sub(before));
        Ok(false)
    }

    /// Finishes the turn at end of input without an end-of-stream frame.
    ///
    /// Complete only when a non-zero stop reason arrived; anything else is a
    /// cut, including a stream that produced tool calls but no stop signal.
    pub(crate) fn finish_eof(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.terminal.is_finished() {
            return Ok(());
        }
        if self.stop.is_none() {
            return Err(self.fail(ProviderError::StreamCut));
        }
        let before = out.len();
        self.finish_success(out)?;
        self.terminal.note_events(out.len().saturating_sub(before));
        Ok(())
    }

    /// Inflates a frame payload when the compressed flag is set.
    fn inflated(&mut self, payload: &[u8], flags: u8) -> Result<Vec<u8>, ProviderError> {
        if flags & COMPRESSED_FLAG == 0 {
            return Ok(payload.to_vec());
        }
        gunzip_capped(payload).map_err(|error| self.fail_connect(&error))
    }

    /// Finishes the turn on an end-of-stream payload.
    fn finish_trailer(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<StreamEvent>,
    ) -> Result<bool, ProviderError> {
        let Some(trailer) = parse_trailer(bytes) else {
            let before = out.len();
            self.finish_success(out)?;
            self.terminal.note_events(out.len().saturating_sub(before));
            return Ok(true);
        };
        Err(self.fail(self.trailer_error(&trailer)))
    }

    /// Maps an error trailer through the retry table.
    ///
    /// The Connect code rides as the in-stream code with no HTTP status, so
    /// known overflow codes become `ContextOverflow`, quota codes become
    /// `Quota`, and anything else becomes `Status` on the stream response.
    /// A trailer naming no code carries the stream response's own status
    /// instead, so an explicit server rejection never becomes a retryable
    /// `Transport`. A failure after the first emitted event is final, never
    /// retried.
    fn trailer_error(&self, trailer: &TrailerError) -> ProviderError {
        let code = (!trailer.code.is_empty()).then_some(trailer.code.as_str());
        let status = trailer.code.is_empty().then_some(IN_STREAM_STATUS);
        let request = RequestState {
            family: Family::Devin,
            provider: &self.provider,
            model: &self.model,
            oauth: false,
            refreshed: false,
            delivered: self.terminal.delivered(),
        };
        match classify(status, code, &trailer.formatted, &request) {
            RetryDecision::Retry(error) | RetryDecision::Fail(error) => error,
            RetryDecision::RefreshOnce => ProviderError::SignInExpired {
                provider: self.provider.to_string(),
            },
        }
    }

    /// Applies one response message to the running turn.
    fn apply(
        &mut self,
        message: &GetChatMessageResponse,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        if self.message_id.is_none() && !message.message_id.is_empty() {
            self.message_id = Some(message.message_id.clone());
        }
        if !message.delta_thinking.is_empty() {
            self.thinking_open = true;
            self.thinking_text.push_str(&message.delta_thinking);
            if !message.delta_signature.is_empty() {
                self.signature = Some(message.delta_signature.clone());
            }
            out.push(StreamEvent::ReasoningDelta {
                text: message.delta_thinking.clone(),
            });
        }
        if !message.delta_text.is_empty() {
            self.close_thinking(out).map_err(|error| self.fail(error))?;
            out.push(StreamEvent::TextDelta {
                text: message.delta_text.clone(),
            });
        }
        if !message.delta_tool_calls.is_empty() {
            self.close_thinking(out).map_err(|error| self.fail(error))?;
            for call in &message.delta_tool_calls {
                self.tool_delta(call, out)?;
            }
        }
        if message.stop_reason != stop_reason::UNSPECIFIED {
            self.stop = Some(message.stop_reason);
        }
        if let Some(usage) = &message.usage {
            self.usage.merge(usage);
        }
        Ok(())
    }

    /// Applies one tool-call delta: opens or continues the call by id,
    /// updates the name, and emits the new argument bytes.
    fn tool_delta(
        &mut self,
        delta: &ChatToolCall,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        let id = if delta.id.is_empty() {
            self.active_id.clone()
        } else {
            Some(delta.id.clone())
        };
        let Some(id) = id else {
            return Ok(());
        };
        let position = self.calls.iter().position(|call| call.id == id);
        let index = if let Some(index) = position {
            index
        } else {
            self.budget.admit_item().map_err(|error| self.fail(error))?;
            self.calls.push(OpenCall {
                id: id.clone(),
                name: String::new(),
                args: Vec::new(),
            });
            out.push(StreamEvent::ToolCallStarted {
                id: id.clone(),
                name: delta.name.clone(),
            });
            self.calls.len().saturating_sub(1)
        };
        let call = &mut self.calls[index];
        if !delta.name.is_empty() {
            call.name.clone_from(&delta.name);
        }
        self.active_id = Some(id.clone());
        if delta.arguments_json.is_empty() {
            return Ok(());
        }
        let previous = String::from_utf8_lossy(&call.args).into_owned();
        let accumulated = if delta.arguments_json.starts_with(&previous) {
            delta.arguments_json.clone()
        } else {
            previous.clone() + &delta.arguments_json
        };
        let fragment = accumulated.as_bytes()[previous.len()..].to_vec();
        call.args = accumulated.into_bytes();
        if fragment.is_empty() {
            return Ok(());
        }
        out.push(StreamEvent::ToolArgsDelta { id, fragment });
        Ok(())
    }

    /// Closes the open thinking run, emitting its signature replay.
    fn close_thinking(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if !self.thinking_open {
            return Ok(());
        }
        self.thinking_open = false;
        let thinking = std::mem::take(&mut self.thinking_text);
        if let Some(signature) = self.signature.take()
            && !signature.is_empty()
        {
            out.push(StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Devin,
                    model: self.model.clone(),
                    item: thinking_replay(&thinking, &signature)?,
                },
            });
        }
        Ok(())
    }

    /// Emits the terminal group: calls, replay, usage, then one stop.
    ///
    /// `MAX_TOKENS` always wins and truncates every call; otherwise open
    /// calls mean `ToolUse`. A message replay rides only on tool-free turns,
    /// because a tool-call response has no closing entry for a capture.
    fn finish_success(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        self.close_thinking(out).map_err(|error| self.fail(error))?;
        let truncating = self.stop == Some(stop_reason::MAX_TOKENS);
        let calls = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|call| ToolCall {
                args: if truncating {
                    ToolArgs::Truncated
                } else if call.args.is_empty() {
                    ToolArgs::from_bytes(b"{}")
                } else {
                    ToolArgs::from_bytes(&call.args)
                },
                id: call.id,
                name: call.name,
            })
            .collect::<Vec<_>>();
        let reason = if truncating {
            StopReason::MaxTokens
        } else if !calls.is_empty() {
            StopReason::ToolUse
        } else {
            self.stop
                .map_or_else(|| StopReason::EndTurn, stop_reason_name)
        };
        let tool_free = calls.is_empty();
        out.push(StreamEvent::ToolCallsDone { calls });
        if tool_free && let Some(id) = self.message_id.take() {
            out.push(StreamEvent::MessageReplay {
                capture: ReplayCapture::Captured {
                    payload: message_replay(&id),
                },
            });
        }
        out.push(StreamEvent::Usage {
            usage: self.usage.usage(),
        });
        out.push(StreamEvent::Stop { reason });
        self.terminal.finish_ok();
        Ok(())
    }
}

/// Maps a stop discriminant onto the neutral stop reason.
fn stop_reason_name(value: i32) -> StopReason {
    match value {
        stop_reason::STOP_PATTERN => StopReason::EndTurn,
        stop_reason::FUNCTION_CALL => StopReason::ToolUse,
        stop_reason::MAX_TOKENS => StopReason::MaxTokens,
        stop_reason::CONTENT_FILTER => StopReason::Refusal,
        _ => StopReason::Other(String::from(stop_reason::name(value))),
    }
}

/// The thinking replay item: full text plus its server signature.
fn thinking_replay(thinking: &str, signature: &str) -> Result<RawJson, ProviderError> {
    #[derive(serde::Serialize)]
    struct ThinkingReplay<'a> {
        thinking: &'a str,
        signature: &'a str,
    }
    let json = sonic_rs::to_string(&ThinkingReplay {
        thinking,
        signature,
    })
    .map_err(|error| ProviderError::Protocol {
        family: Family::Devin,
        detail: format!("devin thinking block did not encode: {error}"),
    })?;
    RawJson::parse(&json).map_err(|error| ProviderError::Protocol {
        family: Family::Devin,
        detail: format!("devin thinking block did not encode: {error}"),
    })
}

/// The message replay item: the server's own response id.
fn message_replay(message_id: &str) -> RawJson {
    #[derive(serde::Serialize)]
    struct MessageReplay<'a> {
        message_id: &'a str,
    }
    sonic_rs::to_string(&MessageReplay { message_id })
        .ok()
        .and_then(|json| RawJson::parse(&json).ok())
        .unwrap_or_else(RawJson::null)
}

/// The driver state behind [`decode_chunks`]: queued events plus the frame
/// parser and the chat decoder. One owner, one queue, no shared state.
struct Driver<S> {
    /// Incoming response bytes.
    chunks: std::pin::Pin<Box<S>>,
    /// Byte-to-frame reassembly.
    frames: FrameDecoder,
    /// Frame-to-event decoding.
    decoder: ChatDecoder,
    /// Decoded events awaiting delivery.
    queue: VecDeque<Result<StreamEvent, ProviderError>>,
}

impl<S> Driver<S>
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    /// A driver over one response body of `model` from `provider`.
    fn new(chunks: S, provider: &str, model: &str) -> Self {
        Self {
            chunks: Box::pin(chunks),
            frames: FrameDecoder::new(),
            decoder: ChatDecoder::new(provider, model),
            queue: VecDeque::new(),
        }
    }

    /// Feeds one network chunk through framing and decoding.
    ///
    /// Frames that precede a corrupt header still decode first; the framing
    /// failure ends the turn only when nothing already did.
    fn pump(&mut self, bytes: &[u8]) {
        let push = self.frames.push(bytes);
        for frame in self.frames.drain() {
            let mut out = Vec::new();
            match self
                .decoder
                .feed_frame(frame.flags, &frame.payload, &mut out)
            {
                Ok(done) => {
                    self.queue.extend(out.into_iter().map(Ok));
                    if done {
                        return;
                    }
                }
                Err(error) => {
                    self.queue.extend(out.into_iter().map(Ok));
                    self.queue.push_back(Err(error));
                    return;
                }
            }
        }
        if let Err(error) = push
            && !self.decoder.is_finished()
        {
            self.queue.push_back(Err(self.decoder.fail_connect(&error)));
        }
    }

    /// Finishes the turn at end of input: a torn frame is a cut, otherwise
    /// the decoder decides between its terminal group and a cut.
    fn finish_input(&mut self) {
        if self.frames.has_partial() {
            self.queue
                .push_back(Err(self.decoder.fail(ProviderError::StreamCut)));
            return;
        }
        let mut out = Vec::new();
        match self.decoder.finish_eof(&mut out) {
            Ok(()) => self.queue.extend(out.into_iter().map(Ok)),
            Err(error) => {
                self.queue.extend(out.into_iter().map(Ok));
                self.queue.push_back(Err(error));
            }
        }
    }
}

/// Decodes a Devin chat byte stream into neutral events.
///
/// Chunks split at any byte boundary; frames reassemble inside. A torn frame
/// at end of input is a cut. Nothing is read after the terminal.
pub(crate) fn decode_chunks<S>(
    chunks: S,
    provider: &str,
    model: &str,
) -> impl Stream<Item = Result<StreamEvent, ProviderError>> + Send + 'static
where
    S: Stream<Item = Vec<u8>> + Send + 'static,
{
    let driver = Driver::new(chunks, provider, model);
    stream::unfold(driver, |mut driver| async move {
        loop {
            if let Some(item) = driver.queue.pop_front() {
                return Some((item, driver));
            }
            if driver.decoder.is_finished() {
                return None;
            }
            if let Some(bytes) = driver.chunks.next().await {
                driver.pump(&bytes);
            } else {
                driver.finish_input();
                if driver.queue.is_empty() {
                    return None;
                }
            }
        }
    })
}

/// One normalized discovery row: the wire facts the catalog join consumes.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "catalog model discovery row mirrors Devin API facts"
)]
pub(crate) struct DiscoveredModel {
    /// Wire uid; the catalog key.
    pub(crate) uid: String,
    /// Display label.
    pub(crate) label: String,
    /// Context window tokens; 0 means unknown.
    pub(crate) max_tokens: i32,
    /// Output token ceiling; 0 means unknown.
    pub(crate) max_output_tokens: i32,
    /// Image input support after the blind-uid policy.
    pub(crate) supports_images: bool,
    /// Server-disabled rows never surface: [`decode_discovery`] skips them
    /// before the row is built, so every returned row reads `false` here.
    /// The flag stays on the row so diagnostics still name the server fact
    /// the filter acted on.
    pub(crate) disabled: bool,
    /// Display slot driving router and visibility handling.
    pub(crate) display_option: i32,
    /// Router marker from features or display slot.
    pub(crate) is_model_router: bool,
    /// Harness uid count; non-empty means a composite, never a router.
    pub(crate) harness_count: usize,
    /// Tool-call support.
    pub(crate) supports_tool_calls: bool,
    /// Parallel tool-call support.
    pub(crate) supports_parallel_tool_calls: bool,
    /// Reasoning support from features or the label heuristic.
    pub(crate) supports_thinking: bool,
    /// Per-million-token input rate.
    pub(crate) cost_input: f64,
    /// Per-million-token output rate.
    pub(crate) cost_output: f64,
    /// Per-million-token cache-read rate.
    pub(crate) cost_cache_read: f64,
    /// Family lane label, when the server declares one.
    pub(crate) family_label: Option<String>,
    /// Family axis entries as `(key, order, name)`.
    pub(crate) family_entries: Vec<(String, i32, String)>,
    /// Family default flag.
    pub(crate) is_family_default: bool,
    /// Server description, when set.
    pub(crate) description: Option<String>,
}

/// Wire uids whose configs advertise images but whose backend silently drops
/// the image field (verified live against the native CLI identity).
const IMAGE_BLIND_UIDS: [&str; 2] = ["swe-1-6", "swe-1-6-fast"];

/// Decodes a `GetCliModelConfigs` body into normalized rows.
///
/// Disabled rows are skipped, empty uids are skipped, and the first row wins
/// a uid. Costs normalize per-million-token rates; the effort ladder stays
/// raw for the catalog join to collapse.
///
/// # Errors
///
/// [`ProviderError::Protocol`] when the body decodes as neither raw nor
/// gzipped protobuf.
pub(crate) fn decode_discovery(bytes: &[u8]) -> Result<Vec<DiscoveredModel>, ProviderError> {
    use super::connect::{ConnectError, decode_unary};
    let response: GetCliModelConfigsResponse = decode_unary(bytes, "GetCliModelConfigsResponse")
        .map_err(|error| match error {
            ConnectError::Decode { detail, .. } => ProviderError::Protocol {
                family: Family::Devin,
                detail,
            },
            other => ProviderError::Protocol {
                family: Family::Devin,
                detail: other.to_string(),
            },
        })?;
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for config in &response.client_model_configs {
        let uid = config.model_uid.trim();
        if config.disabled || uid.is_empty() || !seen.insert(uid.to_owned()) {
            continue;
        }
        let features = config
            .model_info
            .as_ref()
            .and_then(|info| info.model_features.as_ref());
        let display = config
            .model_info
            .as_ref()
            .map_or(0, |info| info.display_option);
        let harness_count = config
            .model_info
            .as_ref()
            .map_or(0, |info| info.harness_uids.len());
        let is_model_router = display == super::proto::display_option::MODEL_ROUTER
            || config
                .model_info
                .as_ref()
                .is_some_and(|info| info.is_model_router);
        let supports_images = features
            .map_or(config.supports_images, |flags| flags.supports_images)
            && !IMAGE_BLIND_UIDS.contains(&uid);
        let (cost_input, cost_output, cost_cache_read) = model_cost(config);
        let (family_label, family_entries, is_family_default) = family_axes(config);
        models.push(DiscoveredModel {
            uid: uid.to_owned(),
            label: {
                let label = config.label.trim();
                if label.is_empty() {
                    uid.to_owned()
                } else {
                    label.to_owned()
                }
            },
            max_tokens: config.max_tokens,
            max_output_tokens: config
                .model_info
                .as_ref()
                .map_or(0, |info| info.max_output_tokens),
            supports_images,
            disabled: config.disabled,
            display_option: display,
            is_model_router,
            harness_count,
            // Absent features mean unknown, not incapable: the catalog
            // default-allows tools and narrows to `None` only on an explicit
            // server `false`, so a missing block must not deny tool routes by
            // itself. Parallel calls default the other way — denied unless
            // confirmed — because the chat wire emits parallel calls on this
            // flag and an unconfirmed `true` risks a server rejection.
            supports_tool_calls: features.is_none_or(|flags| flags.supports_tool_calls),
            supports_parallel_tool_calls: features
                .is_some_and(|flags| flags.supports_parallel_tool_calls),
            supports_thinking: supports_thinking(config),
            cost_input,
            cost_output,
            cost_cache_read,
            family_label,
            family_entries,
            is_family_default,
            description: config
                .description
                .as_ref()
                .map(|description| description.trim().to_owned())
                .filter(|description| !description.is_empty()),
        });
    }
    Ok(models)
}

/// Per-million-token rates from the config's cost dimensions.
///
/// `COST_FUZZY` marks an estimate, not a different unit, so both kinds read.
/// A `sidekick` marker dimension ends the card: composite configs flatten
/// component cards after their own, and the headline card is what budgets
/// against. Values arrive as protobuf floats, so float32 noise rounds off at
/// sub-cent precision.
fn model_cost(config: &ClientModelConfig) -> (f64, f64, f64) {
    let mut input = 0.0;
    let mut output = 0.0;
    let mut cache_read = 0.0;
    for dimension in &config.model_dimensions {
        if dimension.label.trim().eq_ignore_ascii_case("sidekick") {
            break;
        }
        if dimension.kind != model_dimension_kind::COST
            && dimension.kind != model_dimension_kind::COST_FUZZY
        {
            continue;
        }
        let tokens = denominator_tokens(&dimension.denominator);
        let per_million =
            (f64::from(dimension.value) * 1_000_000.0 / tokens * 1_000_000.0).round() / 1_000_000.0;
        match dimension.label.trim().to_lowercase().as_str() {
            "input" => input = per_million,
            "cached input" => cache_read = per_million,
            "output" => output = per_million,
            _ => {}
        }
    }
    (input, output, cache_read)
}

/// Tokens covered by one cost denominator (`"1M tokens"`, `"1K tokens"`).
///
/// Mirrors the pinned pattern `(\d+(?:\.\d+)?)\s*([kmb])?`: the first
/// decimal number, then an optional `k`/`m`/`b` scale after optional
/// whitespace. The scale parses instead of assuming millions, so a future
/// `1K` denominator still normalizes. A denominator with no positive number
/// covers a million.
fn denominator_tokens(denominator: &str) -> f64 {
    let bytes = denominator.as_bytes();
    let Some(start) = bytes.iter().position(u8::is_ascii_digit) else {
        return 1_000_000.0;
    };
    let digits_end = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut end = digits_end(start);
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = digits_end(end + 1);
    }
    let amount: f64 = denominator[start..end].parse().unwrap_or(0.0);
    let scale = match denominator[end..]
        .trim_start()
        .chars()
        .next()
        .map(|point| point.to_ascii_lowercase())
    {
        Some('k') => 1_000.0,
        Some('m') => 1_000_000.0,
        Some('b') => 1_000_000_000.0,
        _ => 1.0,
    };
    let tokens = amount * scale;
    if tokens > 0.0 { tokens } else { 1_000_000.0 }
}

/// Whether the row supports reasoning: server features win, else the label.
///
/// The label heuristic matches the pinned reference's wording set, one whole
/// word at a time: a bare substring match over-claims (`max` fires inside
/// `Maximum`, `low` inside `allow` or `flowing`), so the label is split on
/// non-alphanumeric boundaries first. The word set itself is unchanged —
/// still the pinned wording, no additions, no removals — but the two halves
/// match differently: the effort-ladder names need an exact word, while the
/// `think`/`reasoning` stems match at a word start so `Thinking` still hits
/// without letting `Maximum` through.
fn supports_thinking(config: &ClientModelConfig) -> bool {
    const STEMS: [&str; 2] = ["think", "reasoning"];
    const LADDER: [&str; 6] = ["minimal", "high", "medium", "low", "xhigh", "max"];
    if let Some(features) = config
        .model_info
        .as_ref()
        .and_then(|info| info.model_features.as_ref())
    {
        return features.supports_thinking;
    }
    let label = config.label.to_lowercase();
    if label.contains("no thinking") {
        return false;
    }
    label
        .split(|point: char| !point.is_ascii_alphanumeric())
        .any(|word| LADDER.contains(&word) || STEMS.iter().any(|stem| word.starts_with(*stem)))
}

/// The family lane label and raw axis entries for the catalog join.
fn family_axes(config: &ClientModelConfig) -> (Option<String>, Vec<(String, i32, String)>, bool) {
    let Some(metadata) = config.model_family_metadata.as_ref() else {
        return (None, Vec::new(), false);
    };
    let label = metadata.model_family_label.trim();
    let label = (!label.is_empty()).then(|| label.to_owned());
    let mut entries = Vec::with_capacity(metadata.entries.len());
    for entry in &metadata.entries {
        let Some(value) = entry.value.as_ref() else {
            continue;
        };
        entries.push((entry.key.clone(), value.order, value.name.clone()));
    }
    (
        label,
        entries,
        config.is_default_model_in_family || metadata.is_default_model_in_family,
    )
}
