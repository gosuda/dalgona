//! The Cursor run state machine: server messages to neutral events.
//!
//! [`Session`] owns one `Run` RPC from the first frame to its exactly-one
//! terminal outcome. It decodes
//! [`AgentServerMessage`](super::proto::AgentServerMessage)s into
//! [`StreamEvent`](crate::stream::StreamEvent)s, answers everything the
//! server solicits without host action (KV blobs, request context, approval
//! probes, allowlist prechecks, empty catalogs), and refuses everything else
//! in band. Model-issued MCP calls are the only frames that become tool
//! events; the host answers them through [`CursorTransport::send`] and the
//! results return on the still-open stream.
//!
//! [`RunStream`] pumps response bytes through the session without spawning:
//! `tokio::spawn` is forbidden in this crate, so heartbeats tick from a
//! pinned timer inside the poll, and step boundaries close on poll
//! quiescence (pending calls, quiet byte source, no partial frame) or on a
//! checkpoint that arrives while calls are unflushed. [`open_run`] opens the
//! HTTP exchange over the existing transport and returns the event stream
//! plus the run transport for the join to hand to
//! [`ProviderRun`](crate::run::ProviderRun).
//!
//! While the run awaits host results the pump idles inside [`RunStream`]'s
//! [`RunEvents`](crate::run::RunEvents) upkeep: heartbeats tick and
//! solicitations are answered without yielding step events, and the keeper
//! never polls while a step stream is live.
//!
//! Approval policy, stated once: nothing runs on the host except through
//! dal approval and execution. Approval probes, allowlist prechecks, and
//! every interaction query are refused; native exec frames are refused or
//! thrown, never executed; an unknown exec variant is thrown in band so the
//! server is not stranded and the run goes on, while an unanswerable query
//! ends the run. The reference's auto-approve path is deliberately absent.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll},
};

use bytes::Bytes;
use dal_core::{Family, ModelRequest, Part, ReplayCapture, ReplayRefusal, Usage};
use futures::{Stream, channel::mpsc};
use prost::Message as _;

#[expect(
    clippy::wildcard_imports,
    reason = "Cursor protobuf types are namespaced under proto and imported wholesale"
)]
use super::proto::*;
use super::{
    frame::{
        END_STREAM_FLAG, Frame, FrameDecoder, FrameError, MAX_FRAME_BYTES, decode_message,
        map_connect_error, map_status,
    },
    lower::{
        CursorHistory, CursorRouteParts, PROVIDER_IDENTIFIER, TurnDigest, WireModel, build_run,
        normalize_call_id,
    },
};
use crate::{
    error::{LimitError, ProviderError},
    http,
    run::{HostToolResult, RunEvents, RunLostReason, RunTransport},
    stream::{StopReason, StreamEvent, ToolArgs, ToolCall},
    stream_budget::StreamBudget,
    tool_names::{ToolNames, WIRE_NAME_MAX, wire_name},
};

/// Denial text for approval probes: the probe asks whether the call would
/// run unasked, and this client never runs unasked.
const PROBE_DENIAL: &str = "dal does not approve tool calls without asking.";

/// Refusal text for native exec frames: dal tools run through approval;
/// Cursor-native tools never run on this host.
const NATIVE_REFUSAL: &str = "dal runs tools only through its approved tool set; this native tool is not executed by this client.";

/// Refusal text for hosted interaction queries.
const QUERY_REFUSAL: &str = "dal does not approve hosted actions without asking.";

/// Refusal text for interactive questions.
const ASK_REFUSAL: &str = "Interactive questions are not implemented by this client.";

/// Refusal text for mode switches.
const MODE_REFUSAL: &str = "Mode switches are not implemented by this client.";

/// Error text for plan-file creation.
const PLAN_REFUSAL: &str = "Plan files are not implemented by this client.";

/// Outbox capacity in frames; `send` fails closed past it, never silently.
const OUTBOX_CAPACITY: usize = 64;

/// Largest total of server-set blob bytes one session retains.
const BLOB_STORE_LIMIT: usize = 64 << 20;

/// The user-record prefix the capture walk recognizes.
const USER_RECORD_PREFIX: &str = "{\"role\":\"user\"";

/// Decoded output of one session operation: neutral events plus the client
/// frames the transport must write.
#[derive(Debug, Default)]
pub(crate) struct PumpOut {
    /// Neutral events in emission order.
    pub(crate) events: Vec<StreamEvent>,
    /// Framed client messages, flags unset unless noted.
    pub(crate) writes: Vec<Vec<u8>>,
}

/// One exec route waiting on a result: the numeric id plus the attach id.
#[derive(Clone, Debug)]
struct ExecRoute {
    /// The numeric exec id the reply echoes.
    id: u32,
    /// The attachable execution id, echoed when the request carried one.
    exec_id: String,
    /// The server aborted this route; its result is dropped, never sent.
    aborted: bool,
}

/// One dal-routed call, from its exec frame to its result.
#[derive(Debug)]
struct PendingCall {
    /// The wire call id results pair under.
    wire_id: String,
    /// The internal tool name the events, the digest, and the run cover use.
    internal_name: String,
    /// Canonical argument text displayed so far.
    display: String,
    /// Authoritative arguments from the completion frame, when seen.
    final_args: Option<String>,
    /// Every live exec route for this call, in arrival order.
    routes: Vec<ExecRoute>,
    /// The assembled arguments are not one JSON value.
    invalid: Option<String>,
}

/// One streamed tool-call announcement, kept until its completion for
/// post-exec argument correlation.
#[derive(Debug)]
struct AnnouncedCall {
    /// The wire call id from the streamed call.
    wire_id: String,
    /// The internal tool name.
    internal_name: String,
    /// An exec frame consumed this announcement.
    consumed: bool,
}

/// The constructor bundle for [`Session`]; a struct because the session
/// takes ten inputs and the lint budget allows no more.
pub(crate) struct SessionConfig {
    /// The provider id for error reports.
    pub(crate) provider: Box<str>,
    /// Wire name to internal name, from the advertised tools.
    pub(crate) restore: HashMap<Box<str>, Box<str>>,
    /// Advertised wire names for not-found reports.
    pub(crate) wire_names: Vec<String>,
    /// Advertised tools for the request-context handshake.
    pub(crate) tools: Vec<McpToolDefinition>,
    /// Request-context rules for the handshake.
    pub(crate) rules: Vec<CursorRule>,
    /// History blob id to bytes, seeding the KV store.
    pub(crate) blobs: BTreeMap<Vec<u8>, Vec<u8>>,
    /// The digest seed: route plus the active user message, if any.
    pub(crate) digest: TurnDigest,
    /// Whether the run opened with a fresh user message.
    pub(crate) has_active_user: bool,
}

/// One Cursor run: frame decode, step machine, capture, and replies.
///
/// The session never touches the network or the clock; [`RunStream`] pumps
/// bytes through it and [`CursorTransport`] delivers host results to it.
/// Every method is synchronous and returns what the pump must emit and
/// write; terminal failures surface once through the pump output and the
/// session ignores everything afterwards.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Cursor session state tracks multiple synchronous stream latches"
)]
pub(crate) struct Session {
    /// The provider id for error reports.
    provider: Box<str>,
    /// Wire name to internal name.
    restore: HashMap<Box<str>, Box<str>>,
    /// Advertised wire names for not-found reports.
    wire_names: Vec<String>,
    /// Advertised tools for the request-context handshake.
    tools: Vec<McpToolDefinition>,
    /// Request-context rules for the handshake.
    rules: Vec<CursorRule>,
    /// Blob id to bytes: request blobs plus server-set blobs.
    blobs: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Bytes the server stored via `setBlob`; request blobs are exempt.
    server_blob_bytes: usize,
    /// The route parts for digest and replay comparison.
    route: CursorRouteParts,
    /// This run's request id, stamped on the user record.
    request_id: String,
    /// The running turn digest.
    digest: TurnDigest,
    /// Whether the run opened with a fresh user message.
    has_active_user: bool,
    /// Envelope id to streamed announcement, until its completion.
    announced: HashMap<String, AnnouncedCall>,
    /// Confirmed calls not yet flushed into a step, in arrival order.
    pending: Vec<PendingCall>,
    /// Flushed calls awaiting host results, by wire id.
    awaiting: HashMap<String, PendingCall>,
    /// Wire ids whose results were delivered.
    answered: HashSet<String>,
    /// Ordered text and reasoning runs of the open step.
    runs: Vec<StepRun>,
    /// The last content event was text (for the cut leniency rule).
    tail_text: bool,
    /// Open streamed announcements (for the cut leniency rule).
    open_streamed: usize,
    /// The final step closed cleanly; only usage and a repeat checkpoint
    /// may follow.
    step_completed_is_latest: bool,
    /// Normalized call ids of every tool step this turn, in arrival order.
    turn_calls: Vec<String>,
    /// The last checkpoint roots, when one arrived.
    checkpoint: Option<Vec<Vec<u8>>>,
    /// The checkpoint arrived after every client write of the turn.
    checkpoint_covers: bool,
    /// Authoritative turn usage from `turnEnded`.
    turn_usage: Option<Usage>,
    /// The stream budget for event accounting.
    budget: StreamBudget,
    /// The terminal is emitted; everything further is ignored.
    done: bool,
    /// The transport was released; the pump drains and ends.
    released: bool,
    /// Terminal failure, delivered once.
    failure: Option<ProviderError>,
    /// Incremental frame decoder.
    frames: FrameDecoder,
}

/// One ordered text or reasoning run of the open step, mirroring how the
/// fold coalesces adjacent deltas into blocks.
#[derive(Debug)]
enum StepRun {
    /// Visible text.
    Text(String),
    /// Reasoning text.
    Thinking(String),
}

impl Session {
    /// Builds a session over one lowered run.
    pub(crate) fn new(config: SessionConfig, route: CursorRouteParts, request_id: String) -> Self {
        Self {
            provider: config.provider,
            restore: config.restore,
            wire_names: config.wire_names,
            tools: config.tools,
            rules: config.rules,
            blobs: config.blobs,
            server_blob_bytes: 0,
            route,
            request_id,
            digest: config.digest,
            has_active_user: config.has_active_user,
            announced: HashMap::new(),
            pending: Vec::new(),
            awaiting: HashMap::new(),
            answered: HashSet::new(),
            runs: Vec::new(),
            tail_text: false,
            open_streamed: 0,
            step_completed_is_latest: false,
            turn_calls: Vec::new(),
            checkpoint: None,
            checkpoint_covers: false,
            turn_usage: None,
            budget: StreamBudget::default(),
            done: false,
            released: false,
            failure: None,
            frames: FrameDecoder::new(),
        }
    }

    /// Whether the terminal was emitted.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Whether the transport was released.
    pub(crate) fn is_released(&self) -> bool {
        self.released
    }

    /// The running turn digest, hex-encoded. Test seam for the digest
    /// golden: production reads the digest through the turn capture.
    #[cfg(test)]
    pub(crate) fn digest_hex(&self) -> String {
        self.digest.hex()
    }

    /// Marks the session released: no terminal is emitted for the
    /// release itself; the pump drains its queue and ends.
    pub(crate) fn release(&mut self) {
        self.released = true;
    }

    /// Buffered frame bytes that never formed a frame.
    pub(crate) fn frames_pending(&self) -> usize {
        self.frames.pending()
    }

    /// Feeds response bytes through the frame decoder and the message
    /// state machine, returning events, client writes, and at most one
    /// terminal failure.
    pub(crate) fn on_bytes(&mut self, chunk: &[u8]) -> (PumpOut, Option<ProviderError>) {
        let mut out = PumpOut::default();
        if self.done {
            return (out, None);
        }
        if let Err(error) = self.budget.admit_event(chunk.len()) {
            return self.fail(error, &mut out);
        }
        let mut frames = Vec::new();
        // Frames decoded ahead of a framing failure are real server output:
        // they are handled first, as the reference decoder yields them before
        // it throws, and the failure then ends the run.
        let framing = self.frames.push(chunk, &mut frames);
        for frame in frames {
            if self.done {
                break;
            }
            let ((), failure) = self.on_frame(&frame, &mut out);
            if let Some(error) = failure {
                return self.fail(error, &mut out);
            }
        }
        if let Err(error) = framing
            && !self.done
        {
            let mapped = match error {
                FrameError::TooLarge { .. } => ProviderError::Limit(LimitError::StreamBytes),
                FrameError::Gzip { message } | FrameError::MalformedTrailer { message } => {
                    ProviderError::Protocol {
                        family: Family::Cursor,
                        detail: message.into_string(),
                    }
                }
            };
            return self.fail(mapped, &mut out);
        }
        // Handlers close the session by storing their failure; it leaves
        // with the events decoded before it, never on a later call.
        let failure = self.take_failure();
        (out, failure)
    }

    /// Records the terminal failure and closes the session: the pump
    /// delivers the queued events, then this error, then nothing.
    fn fail(
        &mut self,
        error: ProviderError,
        out: &mut PumpOut,
    ) -> (PumpOut, Option<ProviderError>) {
        self.done = true;
        self.failure = Some(error);
        let mut done = PumpOut::default();
        std::mem::swap(&mut done.events, &mut out.events);
        std::mem::swap(&mut done.writes, &mut out.writes);
        (done, self.failure.take())
    }

    /// Handles one frame: end-of-stream trailers finish the stream,
    /// anything else decodes as a server message.
    fn on_frame(&mut self, frame: &Frame, out: &mut PumpOut) -> ((), Option<ProviderError>) {
        if frame.flags & END_STREAM_FLAG != 0 {
            return self.on_end_stream(&frame.payload, out);
        }
        let message: AgentServerMessage = match decode_message("server message", &frame.payload) {
            Ok(message) => message,
            Err(error) => return ((), Some(error)),
        };
        self.on_message(message, out);
        ((), None)
    }

    /// Handles one end-of-stream trailer: a carried error fails the run
    /// unless the terminal already went out; a clean trailer finishes an
    /// unfinished stream through the cut leniency rule.
    fn on_end_stream(&mut self, payload: &[u8], out: &mut PumpOut) -> ((), Option<ProviderError>) {
        let trailer = match super::frame::parse_trailer(payload) {
            Ok(trailer) => trailer,
            Err(error) => {
                return (
                    (),
                    Some(ProviderError::Protocol {
                        family: Family::Cursor,
                        detail: error.to_string(),
                    }),
                );
            }
        };
        let Some(error) = trailer else {
            return self.on_stream_end(out);
        };
        if self.done {
            return ((), None);
        }
        ((), Some(map_connect_error(self.provider.as_ref(), &error)))
    }

    /// Finishes an unfinished stream at a clean trailer or EOF: the cut
    /// leniency rule keeps a turn that closed cleanly after its final
    /// step, and anything else is a cut.
    fn on_stream_end(&mut self, out: &mut PumpOut) -> ((), Option<ProviderError>) {
        if self.done {
            return ((), None);
        }
        if self.leniency_holds() {
            self.finalize(out);
            return ((), None);
        }
        ((), Some(ProviderError::StreamCut))
    }

    /// Whether a stream that ended without `turnEnded` still counts: the
    /// final step closed, the tail block is text, and no call is open. Bytes
    /// of an unfinished trailing frame do not count against it, as in the
    /// reference: after the closed final step only usage and a repeat
    /// checkpoint can follow.
    fn leniency_holds(&self) -> bool {
        self.step_completed_is_latest
            && self.tail_text
            && self.pending.is_empty()
            && self.awaiting.is_empty()
            && self.announced.values().all(|entry| entry.consumed)
    }

    /// Notes a frame that may extend the turn's records: the final step has
    /// not just closed, and a checkpoint taken earlier no longer covers the
    /// turn. Blob traffic, keepalives, usage, step ends, and the turn end
    /// are exempt; a frame this client does not model is never exempt.
    fn note_progress(&mut self) {
        self.step_completed_is_latest = false;
        self.checkpoint_covers = false;
    }

    /// Dispatches one server message; progress frames clear the
    /// step-completed marker and the checkpoint coverage, checkpoints may
    /// close a tool step, and solicitations are answered inline.
    fn on_message(&mut self, message: AgentServerMessage, out: &mut PumpOut) {
        if self.done {
            return;
        }
        let Some(body) = message.message else {
            self.note_progress();
            return;
        };
        match body {
            server_message::Body::InteractionUpdate(update) => self.on_update(update, out),
            server_message::Body::ExecServerMessage(exec) => self.on_exec(exec, out),
            server_message::Body::ExecServerControlMessage(control) => {
                self.note_progress();
                if let Some(exec_server_control::Body::Abort(abort)) = control.message {
                    self.abort_routes(abort.id);
                }
            }
            server_message::Body::Checkpoint(checkpoint) => self.on_checkpoint(checkpoint, out),
            server_message::Body::KvServerMessage(kv) => self.on_kv(kv, out),
            server_message::Body::InteractionQuery(query) => self.on_query(query, out),
        }
    }

    /// Dispatches one interaction update. Only a heartbeat passes through
    /// untouched: a token estimate keeps the checkpoint coverage but ends
    /// the closed-step marker, a step end sets the marker, and every other
    /// arm, modeled or not, is progress the records may not yet hold.
    fn on_update(&mut self, update: InteractionUpdate, out: &mut PumpOut) {
        let Some(body) = update.update else {
            self.note_progress();
            return;
        };
        match body {
            interaction_update::Update::Heartbeat(_) => {}
            interaction_update::Update::TextDelta(delta) => {
                self.note_progress();
                self.push_text(&delta.text, out);
            }
            interaction_update::Update::ThinkingDelta(delta) => {
                self.note_progress();
                self.push_thinking(&delta.text, out);
            }
            interaction_update::Update::ThinkingCompleted(_)
            | interaction_update::Update::UserMessageAppended(_)
            | interaction_update::Update::StepStarted(_) => self.note_progress(),
            interaction_update::Update::ToolCallStarted(started) => {
                self.note_progress();
                self.tail_text = false;
                self.on_started(started);
            }
            interaction_update::Update::PartialToolCall(partial) => {
                self.note_progress();
                self.on_partial(&partial, out);
            }
            interaction_update::Update::ToolCallDelta(delta) => {
                self.note_progress();
                Self::on_delta(&delta);
            }
            interaction_update::Update::ToolCallCompleted(completed) => {
                self.note_progress();
                self.tail_text = false;
                self.on_completed(&completed);
            }
            interaction_update::Update::TokenDelta(_) => {
                self.step_completed_is_latest = false;
            }
            interaction_update::Update::StepCompleted(_) => {
                self.step_completed_is_latest = true;
            }
            interaction_update::Update::TurnEnded(ended) => self.on_turn_ended(&ended, out),
        }
    }

    /// Appends text to the open run, opening one on a boundary.
    fn push_text(&mut self, text: &str, out: &mut PumpOut) {
        if text.is_empty() {
            return;
        }
        self.tail_text = true;
        match self.runs.last_mut() {
            Some(StepRun::Text(run)) => run.push_str(text),
            Some(StepRun::Thinking(_)) | None => self.runs.push(StepRun::Text(text.to_owned())),
        }
        out.events.push(StreamEvent::TextDelta {
            text: text.to_owned(),
        });
    }

    /// Appends reasoning to the open run, opening one on a boundary.
    fn push_thinking(&mut self, text: &str, out: &mut PumpOut) {
        if text.is_empty() {
            return;
        }
        self.tail_text = false;
        match self.runs.last_mut() {
            Some(StepRun::Thinking(run)) => run.push_str(text),
            Some(StepRun::Text(_)) | None => self.runs.push(StepRun::Thinking(text.to_owned())),
        }
        out.events.push(StreamEvent::ReasoningDelta {
            text: text.to_owned(),
        });
    }

    /// Records a streamed tool-call announcement: MCP calls wait for their
    /// exec frame, a server-side `connect_scm` call loses the run, and
    /// every other server-resolved call stays invisible.
    fn on_started(&mut self, started: ToolCallStartedUpdate) {
        let Some(call) = started.tool_call else {
            return;
        };
        let Some(kind) = self.classify_streamed(&call) else {
            return;
        };
        match kind {
            StreamedCall::Authority(what) => {
                self.done = true;
                self.failure = Some(ProviderError::RunLost {
                    family: Family::Cursor,
                    reason: RunLostReason::Authority(what.into()),
                });
            }
            StreamedCall::Mcp { wire_id, internal } => {
                self.open_streamed += 1;
                self.announced.insert(
                    started.call_id,
                    AnnouncedCall {
                        wire_id,
                        internal_name: internal,
                        consumed: false,
                    },
                );
            }
        }
    }

    /// Classifies one streamed call: dal-routed or server-acted.
    fn classify_streamed(&self, call: &super::proto::ToolCall) -> Option<StreamedCall> {
        let Some(tool_call::Tool::McpToolCall(mcp)) = &call.tool else {
            return Self::classify_native_streamed(call);
        };
        let args = mcp.args.as_ref()?;
        // A streamed announcement for a foreign provider is not ours: leave
        // it invisible like any other server-resolved call. Its exec frame,
        // if any, answers `ToolNotFound` there.
        if !self.is_ours_provider(args.provider_identifier.as_str()) {
            return None;
        }
        Some(StreamedCall::Mcp {
            wire_id: args.tool_call_id.clone(),
            internal: self.restore_name(&args.tool_name, &args.name),
        })
    }

    /// Classifies a non-MCP streamed call: the SCM connector acts on the
    /// account without a permission channel, and every other hosted or
    /// native call resolves server-side.
    fn classify_native_streamed(call: &super::proto::ToolCall) -> Option<StreamedCall> {
        match &call.tool {
            Some(tool_call::Tool::ConnectScm(_)) => Some(StreamedCall::Authority("connect_scm")),
            Some(tool_call::Tool::McpToolCall(_)) | None => None,
        }
    }

    /// Whether an inbound MCP frame names this client.
    ///
    /// The request advertises every dal tool with provider
    /// [`PROVIDER_IDENTIFIER`](super::lower::PROVIDER_IDENTIFIER); history
    /// calls carry that identifier so they pair with those tools. An empty
    /// identifier is the server's legacy omission (every current test and
    /// history playback sends it) and stays dispatched. Only a non-empty
    /// foreign identifier is rejected. The `server_identifier` carries no
    /// declaration at all: history emits it empty and the advertised
    /// definition has no such field, so it is ignored pending live capture
    /// of what the server actually sends for dal tools.
    fn is_ours_provider(&self, identifier: &str) -> bool {
        if identifier.is_empty() || identifier == PROVIDER_IDENTIFIER {
            return true;
        }
        self.tools
            .iter()
            .any(|tool| tool.provider_identifier == identifier)
    }

    /// Restores a wire tool name to its internal name, passing through
    /// names the request never advertised.
    fn restore_name(&self, tool_name: &str, name: &str) -> String {
        let wire = if tool_name.is_empty() {
            name
        } else {
            tool_name
        };
        self.restore
            .get(wire)
            .map_or_else(|| wire.to_owned(), ToString::to_string)
    }

    /// Folds one cumulative argument snapshot into its call: before its
    /// exec frame the snapshot waits unconsumed; after, the new suffix
    /// streams as one delta.
    fn on_partial(&mut self, partial: &PartialToolCallUpdate, out: &mut PumpOut) {
        let Some(snapshot) = canonical_snapshot(&partial.args_text_delta) else {
            return;
        };
        let Some(wire) = self
            .announced
            .get(&partial.call_id)
            .map(|announced| announced.wire_id.clone())
        else {
            return;
        };
        let Some(call) = self.pending.iter_mut().find(|call| call.wire_id == wire) else {
            return;
        };
        if snapshot.len() <= call.display.len() || !snapshot.starts_with(&call.display) {
            return;
        }
        let suffix = snapshot[call.display.len()..].to_owned();
        call.display = snapshot;
        out.events.push(StreamEvent::ToolArgsDelta {
            id: wire,
            fragment: suffix.into_bytes(),
        });
    }

    /// Streamed argument deltas for edit and shell calls are display-only
    /// in this client and never enter the neutral stream.
    fn on_delta(_delta: &ToolCallDeltaUpdate) {}

    /// Settles one streamed completion: a pending dal call takes its
    /// authoritative arguments, and every other completion only releases
    /// its announcement.
    fn on_completed(&mut self, completed: &ToolCallCompletedUpdate) {
        let announced = self.announced.remove(&completed.call_id);
        if announced.as_ref().is_some_and(|entry| !entry.consumed) {
            self.open_streamed = self.open_streamed.saturating_sub(1);
        }
        let wire = announced.map(|entry| entry.wire_id).or_else(|| {
            completed
                .tool_call
                .as_ref()
                .and_then(Self::streamed_wire_id)
        });
        let Some(wire) = wire else {
            return;
        };
        let Some(call) = completed.tool_call.as_ref() else {
            return;
        };
        let Some(pending) = self.pending.iter_mut().find(|known| known.wire_id == wire) else {
            return;
        };
        if let Some(args) = Self::completion_args(call) {
            pending.final_args = Some(args);
        }
    }

    /// The wire id of a streamed MCP call, if the frame carries one.
    fn streamed_wire_id(call: &super::proto::ToolCall) -> Option<String> {
        match &call.tool {
            Some(tool_call::Tool::McpToolCall(mcp)) => {
                mcp.args.as_ref().map(|args| args.tool_call_id.clone())
            }
            Some(_) | None => None,
        }
    }

    /// The authoritative arguments of a completion frame, canonicalized.
    fn completion_args(call: &super::proto::ToolCall) -> Option<String> {
        let Some(tool_call::Tool::McpToolCall(mcp)) = &call.tool else {
            return None;
        };
        let args = mcp.args.as_ref()?;
        canonical_args(&args.args)
    }

    /// Marks every route of one numeric exec id aborted; results for those
    /// routes are dropped at `send`, never written.
    fn abort_routes(&mut self, id: u32) {
        for call in &mut self.pending {
            for route in &mut call.routes {
                if route.id == id {
                    route.aborted = true;
                }
            }
        }
        for call in self.awaiting.values_mut() {
            for route in &mut call.routes {
                if route.id == id {
                    route.aborted = true;
                }
            }
        }
    }

    /// Takes the terminal failure for delivery, once.
    fn take_failure(&mut self) -> Option<ProviderError> {
        self.failure.take()
    }
}

/// One classified streamed call.
enum StreamedCall {
    /// A dal-routed MCP call waiting for its exec frame.
    Mcp {
        /// The wire call id.
        wire_id: String,
        /// The internal tool name.
        internal: String,
    },
    /// A server-side account action without a permission channel.
    Authority(&'static str),
}

/// Canonicalizes one cumulative argument snapshot; unparseable snapshots
/// wait for the next one, since only the exec frame is authoritative.
fn canonical_snapshot(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let parsed = super::proto::parse_json(text).ok()?;
    Some(super::proto::write_json(&parsed))
}

/// Decodes one argument-map value: protobuf-Value bytes first, JSON text
/// as the reference's fallback. `None` marks the call invalid.
fn decode_arg_value(bytes: &[u8]) -> Option<super::proto::Json> {
    if let Ok(value) = PbValue::decode(bytes)
        && let Ok(json) = super::proto::json_from_pb(&value)
    {
        return Some(json);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    super::proto::parse_json(text).ok()
}

/// Canonicalizes one decoded argument map in wire order; `None` when any
/// value is undecodable.
fn canonical_args(args: &BTreeMap<String, Vec<u8>>) -> Option<String> {
    let mut fields = BTreeMap::new();
    for (key, bytes) in args {
        fields.insert(key.clone(), decode_arg_value(bytes)?);
    }
    Some(super::proto::write_json(&super::proto::Json::Obj(fields)))
}

impl Session {
    /// Writes one exec answer frame.
    fn reply(
        &mut self,
        id: u32,
        exec_id: &str,
        body: exec_client_message::Body,
        out: &mut PumpOut,
    ) {
        let message = ExecClientMessage {
            id,
            exec_id: exec_id.to_owned(),
            message: Some(body),
        };
        self.write_client(client_message::Body::ExecClientMessage(message), out);
    }

    /// Writes one exec control frame (throw, close, or heartbeat).
    fn control(&mut self, body: exec_client_control::Body, out: &mut PumpOut) {
        let message = ExecClientControlMessage {
            message: Some(body),
        };
        self.write_client(client_message::Body::ExecClientControlMessage(message), out);
    }

    /// Frames one client message with unset flags, queuing it for the
    /// transport; an unframable message fails the run loudly.
    fn write_client(&mut self, body: client_message::Body, out: &mut PumpOut) {
        let message = AgentClientMessage {
            message: Some(body),
        };
        if let Ok(frame) = super::frame::encode_message(&message, 0) {
            out.writes.push(frame);
        } else {
            self.done = true;
            self.failure = Some(ProviderError::Protocol {
                family: Family::Cursor,
                detail: String::from("cursor client message exceeds the frame limit"),
            });
        }
    }

    /// Fails one exec frame in band with a throw plus a stream close, the
    /// reference's failure channel for unanswerable frames.
    fn throw(&mut self, id: u32, error: &str, code: &str, out: &mut PumpOut) {
        self.control(
            exec_client_control::Body::Throw(ExecClientThrow {
                id,
                error: error.to_owned(),
                error_code: Some(code.to_owned()),
            }),
            out,
        );
        self.control(
            exec_client_control::Body::StreamClose(ExecClientStreamClose { id }),
            out,
        );
    }

    /// Dispatches one exec request: MCP calls become tool events, the
    /// request-context and catalog frames are answered from session state,
    /// prechecks are denied, and every native frame is refused or thrown,
    /// never executed.
    fn on_exec(&mut self, exec: ExecServerMessage, out: &mut PumpOut) {
        self.note_progress();
        let Some(body) = exec.body else {
            self.throw(
                exec.id,
                "Unknown exec message variant",
                "unknown_exec_variant",
                out,
            );
            return;
        };
        match body {
            exec_server_message::Body::RequestContextArgs(_) => {
                self.reply_context(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::McpArgs(args) => {
                self.on_mcp_exec(exec.id, &exec.exec_id, &args, out);
            }
            exec_server_message::Body::ShellArgs(args) => {
                self.reply_shell_refusal(
                    exec.id,
                    &exec.exec_id,
                    &args.command,
                    &args.working_directory,
                    out,
                );
            }
            exec_server_message::Body::ShellStreamArgs(args) => {
                self.reply_shell_stream_refusal(
                    exec.id,
                    &exec.exec_id,
                    &args.command,
                    &args.working_directory,
                    out,
                );
            }
            exec_server_message::Body::WriteArgs(args) => {
                self.reply_write_refusal(exec.id, &exec.exec_id, &args.path, out);
            }
            exec_server_message::Body::DeleteArgs(args) => {
                self.reply_delete_refusal(exec.id, &exec.exec_id, &args.path, out);
            }
            exec_server_message::Body::GrepArgs(_) => {
                self.reply_grep_error(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::ReadArgs(args) => {
                self.reply_read_refusal(exec.id, &exec.exec_id, &args.path, out);
            }
            exec_server_message::Body::LsArgs(args) => {
                self.reply_ls_refusal(exec.id, &exec.exec_id, &args.path, out);
            }
            exec_server_message::Body::DiagnosticsArgs(args) => {
                self.reply_diagnostics_refusal(exec.id, &exec.exec_id, &args.path, out);
            }
            exec_server_message::Body::FetchArgs(args) => {
                self.reply_fetch_error(exec.id, &exec.exec_id, &args.url, out);
            }
            exec_server_message::Body::ListMcpResourcesArgs(_) => {
                self.reply_list_resources(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::ReadMcpResourceArgs(args) => {
                self.reply_read_resource(exec.id, &exec.exec_id, &args.uri, out);
            }
            exec_server_message::Body::McpStateArgs(_) => {
                self.reply_mcp_state(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::ShellAllowlistPrecheck(_) => {
                self.reply_shell_precheck(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::McpAllowlistPrecheck(_) => {
                self.reply_mcp_precheck(exec.id, &exec.exec_id, out);
            }
            exec_server_message::Body::WebFetchAllowlistPrecheck(_) => {
                self.reply_web_fetch_precheck(exec.id, &exec.exec_id, out);
            }
        }
    }

    /// Answers the rules-and-tools handshake from session state.
    fn reply_context(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::RequestContextResult(RequestContextResult {
                result: Some(request_context_result::Result::Success(
                    RequestContextSuccess {
                        request_context: Some(RequestContext {
                            rules: self.rules.clone(),
                            env: None,
                            tools: self.tools.clone(),
                            file_contents: BTreeMap::new(),
                            // A truthful declaration, not enforcement: every
                            // hosted-web query is refused in band.
                            web_search_enabled: Some(false),
                        }),
                    },
                )),
            }),
            out,
        );
    }

    /// Routes one MCP exec frame: approval probes and unknown tools are
    /// answered at once, and anything else becomes one pending tool event
    /// carrying the internal name. A redispatch for a call the session
    /// already holds only gains a route; the first frame's name wins, so
    /// the announcement, the pending call, and the flushed step agree.
    fn on_mcp_exec(&mut self, id: u32, exec_id: &str, args: &McpArgs, out: &mut PumpOut) {
        // Fail closed only where the declared contract gives authority: a
        // non-empty foreign provider never advertised here answers
        // `ToolNotFound` in band. Empty stays dispatched (legacy omission);
        // `server_identifier` is ignored (no declaration; see
        // `is_ours_provider`).
        if !self.is_ours_provider(args.provider_identifier.as_str()) {
            let name = if args.tool_name.is_empty() {
                args.name.clone()
            } else {
                args.tool_name.clone()
            };
            self.reply(
                id,
                exec_id,
                exec_client_message::Body::McpResult(McpResult {
                    result: Some(mcp_result::Result::ToolNotFound(McpToolNotFound {
                        name,
                        available_tools: self.wire_names.clone(),
                    })),
                }),
                out,
            );
            return;
        }
        if args.smart_mode_approval_only {
            self.reply(
                id,
                exec_id,
                exec_client_message::Body::McpResult(McpResult {
                    result: Some(mcp_result::Result::Rejected(McpRejected {
                        reason: String::from(PROBE_DENIAL),
                    })),
                }),
                out,
            );
            return;
        }
        let name = if args.tool_name.is_empty() {
            args.name.clone()
        } else {
            args.tool_name.clone()
        };
        let internal = if let Some(mapped) = self.restore.get(name.as_str()) {
            mapped.to_string()
        } else if self.wire_names.contains(&name) {
            name.clone()
        } else {
            self.reply(
                id,
                exec_id,
                exec_client_message::Body::McpResult(McpResult {
                    result: Some(mcp_result::Result::ToolNotFound(McpToolNotFound {
                        name,
                        available_tools: self.wire_names.clone(),
                    })),
                }),
                out,
            );
            return;
        };
        let wire_id = args.tool_call_id.clone();
        let route = ExecRoute {
            id,
            exec_id: exec_id.to_owned(),
            aborted: false,
        };
        if let Some(call) = self.pending.iter_mut().find(|call| call.wire_id == wire_id) {
            call.routes.push(route);
            return;
        }
        if let Some(call) = self.awaiting.get_mut(&wire_id) {
            call.routes.push(route);
            return;
        }
        if self.answered.contains(&wire_id) {
            return;
        }
        let mut announced = None;
        for entry in self.announced.values_mut() {
            if entry.wire_id == wire_id && !entry.consumed {
                entry.consumed = true;
                self.open_streamed = self.open_streamed.saturating_sub(1);
                announced = Some(entry.internal_name.clone());
            }
        }
        // The streamed announcement already restored this call's name; reuse
        // it so a redispatch that spells the wire name differently cannot
        // fork the pending call from its announcement.
        let internal = announced.unwrap_or(internal);
        let (display, invalid) = match canonical_args(&args.args) {
            Some(text) => (text, None),
            None => (
                String::from("{}"),
                Some(String::from("mcp arguments are not decodable")),
            ),
        };
        self.tail_text = false;
        out.events.push(StreamEvent::ToolCallStarted {
            id: wire_id.clone(),
            name: internal.clone(),
        });
        out.events.push(StreamEvent::ToolArgsDelta {
            id: wire_id.clone(),
            fragment: display.clone().into_bytes(),
        });
        self.pending.push(PendingCall {
            wire_id,
            internal_name: internal,
            display,
            final_args: None,
            routes: vec![route],
            invalid,
        });
        if let Err(error) = self.budget.admit_item() {
            self.done = true;
            self.failure = Some(error);
        }
    }

    /// Refuses one native shell frame with a typed rejection.
    fn reply_shell_refusal(
        &mut self,
        id: u32,
        exec_id: &str,
        command: &str,
        working_directory: &str,
        out: &mut PumpOut,
    ) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ShellResult(ShellResult {
                result: Some(shell_result::Result::Rejected(ShellRejected {
                    command: command.to_owned(),
                    working_directory: working_directory.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                    is_readonly: false,
                })),
            }),
            out,
        );
    }

    /// Refuses one streaming shell frame with a rejection plus a failure
    /// exit, mirroring the reference's denial sequence.
    fn reply_shell_stream_refusal(
        &mut self,
        id: u32,
        exec_id: &str,
        command: &str,
        working_directory: &str,
        out: &mut PumpOut,
    ) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ShellStream(ShellStream {
                event: Some(shell_stream::Event::Rejected(ShellRejected {
                    command: command.to_owned(),
                    working_directory: working_directory.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                    is_readonly: false,
                })),
            }),
            out,
        );
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ShellStream(ShellStream {
                event: Some(shell_stream::Event::Exit(ShellStreamExit {
                    code: 1,
                    cwd: working_directory.to_owned(),
                    aborted: false,
                })),
            }),
            out,
        );
    }

    /// Refuses one native write frame with a typed rejection.
    fn reply_write_refusal(&mut self, id: u32, exec_id: &str, path: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::WriteResult(WriteResult {
                result: Some(write_result::Result::Rejected(WriteRejected {
                    path: path.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Refuses one native delete frame with a typed rejection.
    fn reply_delete_refusal(&mut self, id: u32, exec_id: &str, path: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::DeleteResult(DeleteResult {
                result: Some(delete_result::Result::Rejected(DeleteRejected {
                    path: path.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Answers one native grep frame with a typed error.
    fn reply_grep_error(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::GrepResult(GrepResult {
                result: Some(grep_result::Result::Error(GrepError {
                    error: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Refuses one native read frame with a typed rejection.
    fn reply_read_refusal(&mut self, id: u32, exec_id: &str, path: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ReadResult(ReadResult {
                result: Some(read_result::Result::Rejected(ReadRejected {
                    path: path.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Refuses one native listing frame with a typed rejection.
    fn reply_ls_refusal(&mut self, id: u32, exec_id: &str, path: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::LsResult(LsResult {
                result: Some(ls_result::Result::Rejected(LsRejected {
                    path: path.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Refuses one native diagnostics frame with a typed rejection.
    fn reply_diagnostics_refusal(&mut self, id: u32, exec_id: &str, path: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::DiagnosticsResult(DiagnosticsResult {
                result: Some(diagnostics_result::Result::Rejected(DiagnosticsRejected {
                    path: path.to_owned(),
                    reason: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Answers one hosted fetch frame with a typed error.
    fn reply_fetch_error(&mut self, id: u32, exec_id: &str, url: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::FetchResult(FetchResult {
                result: Some(fetch_result::Result::Error(FetchError {
                    url: url.to_owned(),
                    error: String::from(NATIVE_REFUSAL),
                })),
            }),
            out,
        );
    }

    /// Answers one resource listing with an empty success: this host holds
    /// no live MCP connections.
    fn reply_list_resources(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ListMcpResourcesResult(ListMcpResourcesExecResult {
                result: Some(list_mcp_resources_result::Result::Success(
                    ListMcpResourcesSuccess {
                        resources: Vec::new(),
                    },
                )),
            }),
            out,
        );
    }

    /// Answers one resource read with not-found: this host holds no live
    /// MCP connections.
    fn reply_read_resource(&mut self, id: u32, exec_id: &str, uri: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ReadMcpResourceResult(ReadMcpResourceExecResult {
                result: Some(read_mcp_resource_result::Result::NotFound(
                    ReadMcpResourceNotFound {
                        uri: uri.to_owned(),
                        reason: String::from("this host holds no live MCP connections"),
                    },
                )),
            }),
            out,
        );
    }

    /// Answers the MCP inventory with an empty success.
    fn reply_mcp_state(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::McpStateResult(McpStateExecResult {
                result: Some(mcp_state_result::Result::Success(McpStateSuccess {
                    servers: Vec::new(),
                })),
            }),
            out,
        );
    }

    /// Denies one shell precheck: nothing is pre-allowlisted.
    fn reply_shell_precheck(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::ShellAllowlistResult(ShellAllowlistPrecheckResult {
                allowlisted: false,
            }),
            out,
        );
    }

    /// Denies one MCP precheck: nothing is pre-allowlisted.
    fn reply_mcp_precheck(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::McpAllowlistResult(McpAllowlistPrecheckResult {
                allowlisted: false,
            }),
            out,
        );
    }

    /// Denies one web-fetch precheck: nothing is pre-allowlisted.
    fn reply_web_fetch_precheck(&mut self, id: u32, exec_id: &str, out: &mut PumpOut) {
        self.reply(
            id,
            exec_id,
            exec_client_message::Body::WebFetchAllowlistResult(WebFetchAllowlistPrecheckResult {
                allowlisted: false,
            }),
            out,
        );
    }

    /// Records one checkpoint: its roots feed the turn capture, and a
    /// checkpoint that arrives while calls are unflushed closes the tool
    /// step the server is waiting on.
    fn on_checkpoint(&mut self, checkpoint: ConversationStateStructure, out: &mut PumpOut) {
        self.checkpoint = Some(checkpoint.root_prompt_messages_json);
        self.checkpoint_covers = true;
        self.flush_tool_step(out);
    }

    /// Answers one blob request from the session store: request blobs plus
    /// server-set blobs. KV replies bypass the coverage flag; they are
    /// idempotent and the server re-requests them on resume.
    fn on_kv(&mut self, kv: KvServerMessage, out: &mut PumpOut) {
        let Some(body) = kv.message else {
            return;
        };
        match body {
            kv_server_message::Body::GetBlob(args) => {
                let data = self.blobs.get(&args.blob_id).cloned();
                self.write_client(
                    client_message::Body::KvClientMessage(KvClientMessage {
                        id: kv.id,
                        message: Some(kv_client_message::Body::GetBlobResult(GetBlobResult {
                            blob_data: data,
                        })),
                    }),
                    out,
                );
            }
            kv_server_message::Body::SetBlob(args) => {
                if self.blobs.contains_key(&args.blob_id) {
                    self.write_client(
                        client_message::Body::KvClientMessage(KvClientMessage {
                            id: kv.id,
                            message: Some(kv_client_message::Body::SetBlobResult(SetBlobResult {})),
                        }),
                        out,
                    );
                    return;
                }
                if args.blob_data.len() > MAX_FRAME_BYTES
                    || self.server_blob_bytes + args.blob_data.len() > BLOB_STORE_LIMIT
                {
                    self.done = true;
                    self.failure = Some(ProviderError::Limit(LimitError::StreamBytes));
                    return;
                }
                self.server_blob_bytes += args.blob_data.len();
                self.blobs.insert(args.blob_id, args.blob_data);
                if let Err(error) = self.budget.admit_item() {
                    self.done = true;
                    self.failure = Some(error);
                    return;
                }
                self.write_client(
                    client_message::Body::KvClientMessage(KvClientMessage {
                        id: kv.id,
                        message: Some(kv_client_message::Body::SetBlobResult(SetBlobResult {})),
                    }),
                    out,
                );
            }
        }
    }

    /// Refuses one interaction query in band with the reference's
    /// rejection spelling. VM setup has no answer and an unnamed query
    /// names nothing to refuse, so both lose the run instead of stranding
    /// the server on a reply that never comes.
    #[expect(
        clippy::too_many_lines,
        reason = "query handler matches every interaction query variant to its wire rejection"
    )]
    fn on_query(&mut self, query: InteractionQuery, out: &mut PumpOut) {
        self.note_progress();
        let Some(body) = query.query else {
            self.done = true;
            self.failure = Some(ProviderError::RunLost {
                family: Family::Cursor,
                reason: RunLostReason::Authority(
                    String::from("unnamed interaction query").into_boxed_str(),
                ),
            });
            return;
        };
        match body {
            interaction_query::Query::WebSearch(_) => {
                self.reply_query_rejection(
                    query.id,
                    interaction_response::Result::WebSearch(WebSearchRequestResponse {
                        result: Some(web_search_response::Result::Rejected(QueryRejected {
                            reason: String::from(QUERY_REFUSAL),
                        })),
                    }),
                    out,
                );
            }
            interaction_query::Query::AskQuestion(_) => {
                let response = AskQuestionInteractionResponse {
                    result: Some(AskQuestionResult {
                        result: Some(ask_question_result::Result::Rejected(AskQuestionRejected {
                            reason: String::from(ASK_REFUSAL),
                        })),
                    }),
                };
                self.write_client(
                    client_message::Body::InteractionResponse(InteractionResponse {
                        id: query.id,
                        result: Some(interaction_response::Result::AskQuestion(response)),
                    }),
                    out,
                );
            }
            interaction_query::Query::SwitchMode(_) => {
                self.reply_query_rejection(
                    query.id,
                    interaction_response::Result::SwitchMode(SwitchModeRequestResponse {
                        result: Some(switch_mode_response::Result::Rejected(QueryRejected {
                            reason: String::from(MODE_REFUSAL),
                        })),
                    }),
                    out,
                );
            }
            interaction_query::Query::ExaSearch(_) => {
                self.reply_query_rejection(
                    query.id,
                    interaction_response::Result::ExaSearch(ExaSearchRequestResponse {
                        result: Some(exa_search_response::Result::Rejected(QueryRejected {
                            reason: String::from(QUERY_REFUSAL),
                        })),
                    }),
                    out,
                );
            }
            interaction_query::Query::ExaFetch(_) => {
                self.reply_query_rejection(
                    query.id,
                    interaction_response::Result::ExaFetch(ExaFetchRequestResponse {
                        result: Some(exa_fetch_response::Result::Rejected(QueryRejected {
                            reason: String::from(QUERY_REFUSAL),
                        })),
                    }),
                    out,
                );
            }
            interaction_query::Query::CreatePlan(_) => {
                let response = CreatePlanRequestResponse {
                    result: Some(CreatePlanResult {
                        result: Some(create_plan_result::Result::Error(CreatePlanError {
                            error: String::from(PLAN_REFUSAL),
                        })),
                    }),
                };
                self.write_client(
                    client_message::Body::InteractionResponse(InteractionResponse {
                        id: query.id,
                        result: Some(interaction_response::Result::CreatePlan(response)),
                    }),
                    out,
                );
            }
            interaction_query::Query::SetupVm(_) => {
                self.done = true;
                self.failure = Some(ProviderError::RunLost {
                    family: Family::Cursor,
                    reason: RunLostReason::Authority(
                        String::from("setup_vm_environment").into_boxed_str(),
                    ),
                });
            }
            interaction_query::Query::WebFetch(_) => {
                self.reply_query_rejection(
                    query.id,
                    interaction_response::Result::WebFetch(WebFetchRequestResponse {
                        result: Some(web_fetch_response::Result::Rejected(QueryRejected {
                            reason: String::from(QUERY_REFUSAL),
                        })),
                    }),
                    out,
                );
            }
        }
    }

    /// Writes one query rejection sharing the `Rejected`-at-field-2 shape.
    fn reply_query_rejection(
        &mut self,
        id: u32,
        result: interaction_response::Result,
        out: &mut PumpOut,
    ) {
        self.write_client(
            client_message::Body::InteractionResponse(InteractionResponse {
                id,
                result: Some(result),
            }),
            out,
        );
    }

    /// Handles the terminal update: pending calls at turn end are a
    /// protocol violation, and anything else finalizes the turn with its
    /// authoritative usage.
    fn on_turn_ended(&mut self, ended: &TurnEndedUpdate, out: &mut PumpOut) {
        if self.done {
            return;
        }
        if !self.pending.is_empty() || !self.awaiting.is_empty() {
            self.done = true;
            self.failure = Some(ProviderError::Protocol {
                family: Family::Cursor,
                detail: String::from("cursor turn ended with unanswered tool calls"),
            });
            return;
        }
        self.turn_usage = Some(map_turn_usage(&ended));
        self.finalize(out);
    }

    /// Emits the final step terminal: the empty tool-call close (pending
    /// calls already ended the run as a protocol error), the capture when
    /// the turn earned one (captured or refused), the usage, and exactly
    /// one stop.
    fn finalize(&mut self, out: &mut PumpOut) {
        if self.done {
            return;
        }
        self.done = true;
        let calls = self.finish_step_calls();
        out.events.push(StreamEvent::ToolCallsDone { calls });
        if let Some(capture) = self.capture_turn_records() {
            out.events.push(StreamEvent::MessageReplay { capture });
        }
        out.events.push(StreamEvent::Usage {
            usage: self.turn_usage.unwrap_or(Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            }),
        });
        out.events.push(StreamEvent::Stop {
            reason: StopReason::EndTurn,
        });
        self.runs.clear();
    }

    /// Closes the open tool step when the server waits on results: one
    /// `ToolCallsDone` with zero usage and a tool-use stop, leaving the
    /// run alive for the host's answer.
    fn flush_tool_step(&mut self, out: &mut PumpOut) {
        if self.done || self.pending.is_empty() {
            return;
        }
        let calls = self.finish_step_calls();
        out.events.push(StreamEvent::ToolCallsDone { calls });
        out.events.push(StreamEvent::Usage {
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                premium_requests: None,
                cost_usd: None,
            },
        });
        out.events.push(StreamEvent::Stop {
            reason: StopReason::ToolUse,
        });
        self.runs.clear();
        self.tail_text = false;
    }

    /// Moves every pending call into awaiting and folds the open runs plus
    /// the calls into the digest, returning the completed calls in arrival
    /// order with wire ids and internal names: the host answers them
    /// verbatim and the run matches them exactly.
    fn finish_step_calls(&mut self) -> Vec<ToolCall> {
        for run in std::mem::take(&mut self.runs) {
            match run {
                StepRun::Text(text) => self.digest.assistant_text(&text),
                StepRun::Thinking(text) => self.digest.assistant_thinking(&text),
            }
        }
        let mut calls = Vec::with_capacity(self.pending.len());
        for call in std::mem::take(&mut self.pending) {
            let args = call_args(&call);
            let args_text = args_text(&args);
            self.digest
                .assistant_tool_call(&call.wire_id, &call.internal_name, &args_text);
            self.turn_calls.push(normalize_call_id(&call.wire_id));
            calls.push(ToolCall {
                id: call.wire_id.clone(),
                name: call.internal_name.clone(),
                args,
            });
            self.awaiting.insert(call.wire_id.clone(), call);
        }
        calls
    }
}

/// The completed arguments of one pending call: the completion frame wins,
/// else the displayed canonical text, else `{}`.
fn call_args(call: &PendingCall) -> ToolArgs {
    if let Some(message) = &call.invalid {
        return ToolArgs::Invalid {
            message: message.clone(),
        };
    }
    let text = call.final_args.as_deref().unwrap_or(&call.display);
    let text = if text.is_empty() { "{}" } else { text };
    ToolArgs::from_bytes(text.as_bytes())
}

/// The argument text a digest folds: parsed raw text, or the invalid
/// message, or the truncation marker.
fn args_text(args: &ToolArgs) -> String {
    match args {
        ToolArgs::Parsed(raw) => raw.as_str().to_owned(),
        ToolArgs::Invalid { message } => message.clone(),
        ToolArgs::Truncated => String::from("truncated"),
    }
}

/// Maps a terminal update onto the core counters: the input counts the
/// whole prompt with cache hits and writes included, the output excludes
/// nothing (reasoning rides separately), and an all-zero frame stays
/// unreported.
fn map_turn_usage(ended: &TurnEndedUpdate) -> Usage {
    let positive = |value: Option<i64>| u64::try_from(value.unwrap_or(0)).unwrap_or(0);
    let reasoning = ended.reasoning_tokens.unwrap_or(0);
    Usage {
        input_tokens: positive(ended.input_tokens),
        cached_input_tokens: positive(ended.cache_read_tokens),
        output_tokens: positive(ended.output_tokens),
        reasoning_tokens: if reasoning > 0 {
            u64::try_from(reasoning).ok()
        } else {
            None
        },
        cache_write_tokens: positive(ended.cache_write_tokens),
        premium_requests: None,
        cost_usd: None,
    }
}

impl Session {
    /// Assembles the turn capture: the checkpoint's records after the
    /// stamped user record, bound to the route and digest.
    ///
    /// Truly absent replay yields no event and the turn rebuilds from
    /// content: no active user, an uncovered checkpoint, no checkpoint at
    /// all, or a payload that cannot be journaled. Malformed present replay
    /// yields a refused capture so the fold fails the turn instead of
    /// silently rebuilding: non-UTF8 bytes, non-JSON records, a checkpoint
    /// referencing unknown blobs, no user record for this request, or no
    /// answer records after it.
    fn capture_turn_records(&self) -> Option<ReplayCapture> {
        if !self.has_active_user || !self.checkpoint_covers {
            return None;
        }
        let roots = self.checkpoint.as_ref()?;
        let mut records: Vec<String> = Vec::new();
        for id in roots.iter().rev() {
            let Some(bytes) = self.blobs.get(id) else {
                return Some(ReplayCapture::Refused {
                    reason: ReplayRefusal::UserRecordNotFound,
                });
            };
            let Ok(text) = std::str::from_utf8(bytes) else {
                return Some(ReplayCapture::Refused {
                    reason: ReplayRefusal::RecordNotUtf8,
                });
            };
            if text.starts_with(USER_RECORD_PREFIX)
                && text.contains(format!("\"requestId\":\"{}\"", self.request_id).as_str())
            {
                if records.is_empty() {
                    return Some(ReplayCapture::Refused {
                        reason: ReplayRefusal::NoRecords,
                    });
                }
                records.reverse();
                let history = CursorHistory::new(
                    &self.route,
                    self.digest.hex(),
                    self.turn_calls.clone(),
                    records,
                );
                // Unreachable for constructed values; drop instead of failing.
                let payload = history.to_payload()?;
                return Some(ReplayCapture::Captured { payload });
            }
            if super::proto::parse_json(text).is_err() {
                return Some(ReplayCapture::Refused {
                    reason: ReplayRefusal::RecordNotJson,
                });
            }
            records.push(text.to_owned());
        }
        Some(ReplayCapture::Refused {
            reason: ReplayRefusal::UserRecordNotFound,
        })
    }

    /// Delivers host results to the open stream: each result is matched to
    /// its awaiting call, folded into the digest, encoded as one
    /// `McpResult` per live route, and written at once. Aborted routes are
    /// skipped; unknown calls lose the run.
    fn send_results(
        &mut self,
        results: &[HostToolResult],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<(), RunLostReason> {
        for result in results {
            let wire = result.call.as_str().to_owned();
            let Some(mut call) = self.awaiting.remove(&wire) else {
                return Err(RunLostReason::Protocol(String::from(
                    "tool result answers no awaiting call",
                )));
            };
            // A delivered result is a client write the checkpoint taken so far
            // cannot hold: only a later checkpoint covers the turn again.
            self.checkpoint_covers = false;
            self.digest
                .tool_result(result.call.as_str(), result.is_error, &result.parts);
            let answer = encode_host_result(result);
            self.answered.insert(wire);
            for route in std::mem::take(&mut call.routes) {
                if route.aborted {
                    continue;
                }
                let message = ExecClientMessage {
                    id: route.id,
                    exec_id: route.exec_id,
                    message: Some(exec_client_message::Body::McpResult(answer.clone())),
                };
                let frame = super::frame::encode_message(
                    &AgentClientMessage {
                        message: Some(client_message::Body::ExecClientMessage(message)),
                    },
                    0,
                )
                .map_err(|_| RunLostReason::BufferExceeded)?;
                out.push(frame);
            }
        }
        Ok(())
    }

    /// Frames the 5 s run keepalive.
    fn heartbeat_frame() -> Vec<u8> {
        super::frame::encode_message(
            &AgentClientMessage {
                message: Some(client_message::Body::ClientHeartbeat(ClientHeartbeat {})),
            },
            0,
        )
        .unwrap_or_default()
    }

    /// Frames a turn-level cancel action for protocol completeness and
    /// tests; production cancellation closes the transport instead, as
    /// the reference does.
    #[cfg(test)]
    pub(crate) fn cancel_frame() -> Vec<u8> {
        super::frame::encode_message(
            &AgentClientMessage {
                message: Some(client_message::Body::ConversationAction(
                    ConversationAction {
                        action: Some(conversation_action::Action::CancelAction(CancelAction {})),
                    },
                )),
            },
            0,
        )
        .unwrap_or_default()
    }
}

/// Encodes one host result as an `McpResult`: content items travel
/// verbatim, errors become the typed error, and a result naming an
/// unresolvable blob becomes an error result rather than a lost run.
fn encode_host_result(result: &HostToolResult) -> McpResult {
    if let Some(blob) = unresolved_blob(&result.parts) {
        return McpResult {
            result: Some(mcp_result::Result::Error(McpError {
                error: format!("tool result blob {blob} was never resolved"),
            })),
        };
    }
    if result.is_error {
        return McpResult {
            result: Some(mcp_result::Result::Error(McpError {
                error: join_text(&result.parts),
            })),
        };
    }
    McpResult {
        result: Some(mcp_result::Result::Success(McpSuccess {
            content: host_content(&result.parts),
            is_error: false,
        })),
    }
}

/// The first unresolved blob id in a result, if any.
fn unresolved_blob(parts: &[Part]) -> Option<String> {
    parts.iter().find_map(|part| match part {
        Part::Blob { blob_id, .. } => Some(blob_id.to_string()),
        Part::Text { .. } | Part::Image { .. } => None,
    })
}

/// Joins text parts with newlines; images note their media type, and an
/// empty result names itself.
fn join_text(parts: &[Part]) -> String {
    let mut texts = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => texts.push(text.to_string()),
            Part::Image { mime, .. } => texts.push(format!("[image {mime}]")),
            Part::Blob { .. } => {}
        }
    }
    let joined = texts.join("\n");
    if joined.is_empty() {
        String::from("(empty result)")
    } else {
        joined
    }
}

/// Converts success content: text verbatim, images with bytes; blobs are
/// unreachable (checked first) and skipped.
fn host_content(parts: &[Part]) -> Vec<McpContentItem> {
    let mut content = Vec::new();
    for part in parts {
        match part {
            Part::Text { text } => content.push(McpContentItem {
                content: Some(mcp_content_item::Content::Text(McpTextContent {
                    text: text.to_string(),
                })),
            }),
            Part::Image { mime, bytes } => content.push(McpContentItem {
                content: Some(mcp_content_item::Content::Image(McpImageContent {
                    data: bytes.to_vec(),
                    mime_type: mime.to_string(),
                })),
            }),
            Part::Blob { .. } => {}
        }
    }
    content
}

/// Locks a session mutex, recovering from poisoning with the surviving
/// state rather than panicking the pump.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One open run's event pump: response bytes in, neutral events out, with
/// heartbeats and step quiescence inside the poll and no spawned task.
///
/// Its [`RunEvents`] upkeep performs the same pump while the run awaits host
/// results, buffering step events for the next step instead of delivering
/// them.
pub(crate) struct RunStream {
    /// The shared session.
    session: Arc<Mutex<Session>>,
    /// The mapped response body.
    body: Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>,
    /// The heartbeat timer.
    heartbeat: Pin<Box<tokio::time::Sleep>>,
    /// Decoded events awaiting delivery.
    ready: VecDeque<Result<StreamEvent, ProviderError>>,
    /// Client frames for the request body.
    out: Option<mpsc::Sender<Result<Bytes, std::io::Error>>>,
    /// The terminal went out; only `None` follows.
    done: bool,
}

impl RunStream {
    /// Builds a pump over one session and body.
    pub(crate) fn new(
        session: Arc<Mutex<Session>>,
        body: Pin<Box<dyn Stream<Item = Result<Bytes, ProviderError>> + Send>>,
        out: mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) -> Self {
        Self {
            session,
            body,
            heartbeat: Box::pin(tokio::time::sleep(super::HEARTBEAT_INTERVAL)),
            ready: VecDeque::new(),
            out: Some(out),
            done: false,
        }
    }

    /// Forwards queued client frames to the request body; a full or closed
    /// outbox ends the stream with a transport error.
    fn forward_writes(&mut self, writes: Vec<Vec<u8>>) -> bool {
        let Some(out) = self.out.as_mut() else {
            return false;
        };
        for frame in writes {
            if out.try_send(Ok(Bytes::from(frame))).is_err() {
                self.ready.push_back(Err(ProviderError::Transport {
                    family: Family::Cursor,
                    reason: String::from("cursor run outbox is full or closed"),
                }));
                return false;
            }
        }
        true
    }

    /// Forwards upkeep frames without touching buffered step events. A
    /// released outbox leaves the ending to the run; a full or closed one
    /// ends the upkeep with a transport error, never silently.
    fn maintain_writes(&mut self, writes: Vec<Vec<u8>>) -> Option<ProviderError> {
        if writes.is_empty() || self.out.is_none() {
            return None;
        }
        if !self.forward_writes(writes) {
            // `forward_writes` queued exactly one transport error.
            if let Some(Err(error)) = self.ready.pop_back() {
                return Some(error);
            }
            debug_assert!(false, "cursor upkeep lost its outbox error");
        }
        None
    }

    /// Drains one pump output into the ready queue and the outbox.
    fn drain(&mut self, pump: PumpOut, failure: Option<ProviderError>) {
        for event in pump.events {
            self.ready.push_back(Ok(event));
        }
        if !self.forward_writes(pump.writes) {
            return;
        }
        if let Some(error) = failure {
            self.ready.push_back(Err(error));
        } else if let Some(error) = lock(&self.session).take_failure() {
            self.ready.push_back(Err(error));
        }
    }
}

impl Stream for RunStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            if let Some(event) = self.ready.pop_front() {
                if event.is_err() {
                    self.done = true;
                }
                return Poll::Ready(Some(event));
            }
            let is_done = {
                let session = lock(&self.session);
                session.is_done() || session.is_released()
            };
            if is_done {
                self.done = true;
                return Poll::Ready(None);
            }
            if self.heartbeat.as_mut().poll(cx).is_ready() {
                let frame = Session::heartbeat_frame();
                self.forward_writes(vec![frame]);
                self.heartbeat = Box::pin(tokio::time::sleep(super::HEARTBEAT_INTERVAL));
                continue;
            }
            match self.body.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    let (pump, failure) = lock(&self.session).on_bytes(&chunk);
                    self.drain(pump, failure);
                }
                Poll::Ready(Some(Err(error))) => {
                    self.ready.push_back(Err(error));
                }
                Poll::Ready(None) => {
                    let (pump, failure) = {
                        let mut session = lock(&self.session);
                        let mut out = PumpOut::default();
                        let ((), failure) = session.on_stream_end(&mut out);
                        (out, failure)
                    };
                    self.drain(pump, failure);
                }
                Poll::Pending => {
                    let (pump, failure) = {
                        let mut session = lock(&self.session);
                        let mut out = PumpOut::default();
                        if !session.pending.is_empty() && session.frames_pending() == 0 {
                            session.flush_tool_step(&mut out);
                        }
                        let failure = session.take_failure();
                        (out, failure)
                    };
                    self.drain(pump, failure);
                    if let Some(event) = self.ready.pop_front() {
                        if event.is_err() {
                            self.done = true;
                        }
                        return Poll::Ready(Some(event));
                    }
                    return Poll::Pending;
                }
            }
        }
    }
}

impl RunEvents for RunStream {
    /// Drives connection upkeep while the run awaits host results: the
    /// heartbeat ticks, response bytes pump through the session, and
    /// solicitations (KV, exec-control, query refusals) are answered in
    /// band. Step events decoded along the way wait in the ready queue for
    /// the next step; they never surface here. A terminal failure ends the
    /// upkeep with the error for the keeper to fold into the run.
    fn poll_maintain(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), ProviderError>> {
        let this = self.get_mut();
        loop {
            if this.done {
                return Poll::Ready(Ok(()));
            }
            {
                let mut session = lock(&this.session);
                if session.is_done() || session.is_released() {
                    // A stored failure still owns the ending; a clean drain
                    // leaves whatever is buffered to the next step.
                    return match session.take_failure() {
                        Some(error) => Poll::Ready(Err(error)),
                        None => Poll::Ready(Ok(())),
                    };
                }
            }
            if this.heartbeat.as_mut().poll(cx).is_ready() {
                let frame = Session::heartbeat_frame();
                if let Some(error) = this.maintain_writes(vec![frame]) {
                    return Poll::Ready(Err(error));
                }
                this.heartbeat = Box::pin(tokio::time::sleep(super::HEARTBEAT_INTERVAL));
                continue;
            }
            match this.body.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    let (pump, failure) = lock(&this.session).on_bytes(&chunk);
                    for event in pump.events {
                        this.ready.push_back(Ok(event));
                    }
                    if let Some(error) = this.maintain_writes(pump.writes) {
                        return Poll::Ready(Err(error));
                    }
                    let stored = lock(&this.session).take_failure();
                    if let Some(error) = failure.or(stored) {
                        return Poll::Ready(Err(error));
                    }
                }
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(None) => {
                    let (pump, failure) = {
                        let mut session = lock(&this.session);
                        let mut out = PumpOut::default();
                        let ((), failure) = session.on_stream_end(&mut out);
                        (out, failure)
                    };
                    for event in pump.events {
                        this.ready.push_back(Ok(event));
                    }
                    // Writes are empty on this path today; fail closed if
                    // that ever changes rather than dropping frames.
                    if let Some(error) = this.maintain_writes(pump.writes) {
                        return Poll::Ready(Err(error));
                    }
                    let stored = lock(&this.session).take_failure();
                    match failure.or(stored) {
                        Some(error) => return Poll::Ready(Err(error)),
                        // Leniency finalized without failure: unreachable
                        // while calls are unanswered, but the buffered
                        // terminal belongs to the next step either way.
                        None => return Poll::Ready(Ok(())),
                    }
                }
                Poll::Pending => {
                    let stored = lock(&this.session).take_failure();
                    return match stored {
                        Some(error) => Poll::Ready(Err(error)),
                        None => Poll::Pending,
                    };
                }
            }
        }
    }
}

/// The connection side of a Cursor run: delivers host results onto the
/// open stream and tears it down on release, loss, or drop.
pub(crate) struct CursorTransport {
    /// The shared session.
    session: Arc<Mutex<Session>>,
    /// Client frames for the request body; dropped on abort.
    out: Option<mpsc::Sender<Result<Bytes, std::io::Error>>>,
}

impl CursorTransport {
    /// Builds a transport over one session and outbox.
    #[cfg(test)]
    pub(crate) fn new(
        session: Arc<Mutex<Session>>,
        out: mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) -> Self {
        Self {
            session,
            out: Some(out),
        }
    }
}

impl RunTransport for CursorTransport {
    fn send(&mut self, results: Vec<HostToolResult>) -> Result<(), RunLostReason> {
        let mut frames = Vec::new();
        {
            let mut session = lock(&self.session);
            session.send_results(&results, &mut frames)?;
        }
        let Some(out) = self.out.as_mut() else {
            return Err(RunLostReason::Transport(String::from(
                "cursor run transport is closed",
            )));
        };
        for frame in frames {
            out.try_send(Ok(Bytes::from(frame)))
                .map_err(|_| RunLostReason::BufferExceeded)?;
        }
        Ok(())
    }

    fn abort(&mut self) {
        lock(&self.session).release();
        self.out = None;
    }
}

/// The inputs for opening one Cursor run.
pub(crate) struct OpenArgs<'a> {
    /// The shared HTTP client.
    pub(crate) client: &'a reqwest::Client,
    /// The provider base URL.
    pub(crate) base_url: &'a str,
    /// The bearer token.
    pub(crate) bearer: &'a str,
    /// The provider id for error reports.
    pub(crate) provider: &'a str,
    /// The user agent string.
    pub(crate) user_agent: &'a str,
    /// The inference request.
    pub(crate) request: &'a ModelRequest,
    /// The catalog wire route.
    pub(crate) wire: &'a WireModel,
    /// The conversation id, minted per run.
    pub(crate) conversation_id: String,
    /// The message id, minted per request.
    pub(crate) message_id: String,
}

/// One opened run: the event pump plus the run transport.
pub(crate) struct OpenRun {
    /// The neutral event stream, every step back to back.
    pub(crate) events: RunStream,
    /// The connection side for results and teardown.
    pub(crate) transport: CursorTransport,
}

/// Opens one Cursor run: lowers the request, sends the framed run request
/// over the existing transport, and returns the pump plus the transport.
///
/// # Errors
///
/// Returns `ToolNameCollision` for colliding advertisements,
/// `InvalidRequest` for an unlowerable request or unusable credential, the
/// mapped status for a rejected run, or `Transport` when the exchange
/// cannot start.
pub(crate) async fn open_run(args: OpenArgs<'_>) -> Result<OpenRun, ProviderError> {
    ToolNames::for_request(args.request, WIRE_NAME_MAX)?;
    let lowered = build_run(
        args.request,
        args.wire,
        &args.conversation_id,
        &args.message_id,
    )?;
    let (restore, wire_names) = name_tables(args.request);
    let session = Arc::new(Mutex::new(Session::new(
        SessionConfig {
            provider: args.provider.into(),
            restore,
            wire_names,
            tools: lowered.tools.clone(),
            rules: lowered.rules.clone(),
            blobs: lowered.blobs,
            digest: lowered.digest,
            has_active_user: lowered.has_active_user,
        },
        lowered.route.clone(),
        args.message_id.clone(),
    )));
    let (mut out_tx, out_rx) = mpsc::channel(OUTBOX_CAPACITY);
    out_tx
        .try_send(Ok(Bytes::from(lowered.first_frame)))
        .map_err(|_| ProviderError::Transport {
            family: Family::Cursor,
            reason: String::from("cursor run outbox is closed"),
        })?;
    let url = http::endpoint(Family::Cursor, args.base_url, super::RUN_PATH)?;
    let bearer = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", args.bearer))
        .map_err(|_| ProviderError::InvalidRequest {
            message: String::from("cursor bearer token is not a header value"),
        })?;
    let request_id = reqwest::header::HeaderValue::from_str(&args.message_id).map_err(|_| {
        ProviderError::InvalidRequest {
            message: String::from("cursor request id is not a header value"),
        }
    })?;
    let response = http::send(
        Family::Cursor,
        args.client
            .post(url)
            .header(reqwest::header::AUTHORIZATION, bearer)
            .header(
                reqwest::header::HeaderName::from_static("x-cursor-client-type"),
                reqwest::header::HeaderValue::from_static(super::CLIENT_TYPE),
            )
            .header(
                reqwest::header::HeaderName::from_static("x-cursor-client-version"),
                reqwest::header::HeaderValue::from_static(super::CLIENT_VERSION),
            )
            .header(
                reqwest::header::HeaderName::from_static("x-ghost-mode"),
                reqwest::header::HeaderValue::from_static("true"),
            )
            .header(
                reqwest::header::HeaderName::from_static("connect-protocol-version"),
                reqwest::header::HeaderValue::from_static(super::PROTOCOL_VERSION),
            )
            .header(
                reqwest::header::HeaderName::from_static("x-request-id"),
                request_id,
            )
            .header(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static(super::CONNECT_PROTO),
            )
            .body(reqwest::Body::wrap_stream(out_rx)),
        args.user_agent,
        http::Exchange::Bytes {
            accept: super::CONNECT_PROTO,
            total: None,
        },
        tokio::time::sleep,
    )
    .await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = http::read_body(Family::Cursor, response).await?;
        return Err(map_status(args.provider, status, &body));
    }
    let body = response.bytes_stream();
    let mapped = futures::StreamExt::map(body, move |chunk| match chunk {
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(if error.is_timeout() {
            ProviderError::StreamCut
        } else {
            http::from_reqwest(Family::Cursor, error)
        }),
    });
    Ok(OpenRun {
        events: RunStream::new(session.clone(), Box::pin(mapped), out_tx.clone()),
        transport: CursorTransport {
            session,
            out: Some(out_tx),
        },
    })
}

/// Builds the wire-to-internal name table from the advertised tools: only
/// mapped names need entries, since every other name passes through.
fn name_tables(request: &ModelRequest) -> (HashMap<Box<str>, Box<str>>, Vec<String>) {
    let mut restore = HashMap::new();
    let mut wire_names = Vec::new();
    for tool in request.tools.iter() {
        let wire = wire_name(tool.name.as_ref(), WIRE_NAME_MAX).to_string();
        wire_names.push(wire.clone());
        if wire.as_str() != tool.name.as_ref() {
            restore.insert(wire.into_boxed_str(), tool.name.clone());
        }
    }
    (restore, wire_names)
}
