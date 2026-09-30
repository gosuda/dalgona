//! The deterministic scripted provider.
//!
//! A [`Script`] is an ordered list of [`ScriptStep`] values. Each provider
//! operation consumes the next step: `open` takes an events step or a fail
//! step, `usage` takes a usage step or a fail step, and `compact` takes a
//! compact step or a fail step. A step of another kind is left in place and
//! the operation fails with [`ScriptError::Mismatch`]; an empty script fails
//! with [`ScriptError::Exhausted`]. No step is skipped, repeated, or invented.
//!
//! Every events step is checked against the neutral stream grammar when the
//! script is built. A complete step is any number of text, reasoning,
//! call-start, argument-fragment, and replay events, then exactly one
//! `ToolCallsDone`, exactly one `Usage`, and exactly one `Stop`, in that
//! order. A step that ends before `Stop` is a failing stream: the fail step
//! that must follow it becomes the stream's error terminal, and both steps
//! are consumed by one `open`. A fail step on its own fails `open` before any
//! stream exists. Events are delivered verbatim; tool arguments are never
//! reassembled or normalized.
//!
//! Streams go through [`EventStream`], so the terminal guard and the
//! at-most-once cancellation action are the ones every family uses. The queue
//! is owned by the script instance; there is no global state. Concurrent
//! operations take steps one at a time under one lock, so each step is
//! delivered exactly once and steps leave in script order.

use std::{
    collections::VecDeque,
    fmt,
    iter::{Enumerate, Peekable},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    vec,
};

use dal_core::{Family, RawJson, Usage};
use futures::stream;
use serde::Deserialize;

use crate::{
    compact::CompactOutcome,
    error::ProviderError,
    stream::{EventStream, StopReason, StreamEvent, ToolArgs, ToolCall},
};

/// One step of a [`Script`].
#[derive(Debug)]
pub enum ScriptStep {
    /// The events of one streamed response, served by `open`.
    ///
    /// Without a final `Stop` the next step must be [`ScriptStep::Fail`],
    /// whose error ends the stream after these events.
    Events(Vec<StreamEvent>),
    /// A failure, returned by the operation that consumes it.
    Fail(ProviderError),
    /// The token usage returned by one `usage` read.
    Usage(Usage),
    /// The result of one remote compaction request.
    Compact(CompactOutcome),
}

/// The kind of a [`ScriptStep`], named in [`ScriptError::Mismatch`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepKind {
    /// [`ScriptStep::Events`].
    Events,
    /// [`ScriptStep::Fail`].
    Fail,
    /// [`ScriptStep::Usage`].
    Usage,
    /// [`ScriptStep::Compact`].
    Compact,
}

impl fmt::Display for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Events => "events",
            Self::Fail => "fail",
            Self::Usage => "usage",
            Self::Compact => "compact",
        })
    }
}

/// A provider operation that consumes a script step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Opening a response stream.
    Open,
    /// Reading token usage.
    Usage,
    /// Requesting remote compaction.
    Compact,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Usage => "usage",
            Self::Compact => "compact",
        })
    }
}

/// A script that cannot be built or cannot serve an operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ScriptError {
    /// An events step breaks the stream grammar.
    #[error("script step {step}: {detail}")]
    Grammar {
        /// The zero-based index of the step in the script.
        step: usize,
        /// The violated rule, in one short phrase.
        detail: &'static str,
    },
    /// No step is left for the operation.
    #[error("script exhausted: no step left for {operation}")]
    Exhausted {
        /// The operation that found the script empty.
        operation: Operation,
    },
    /// The next step does not serve the operation; it stays in the script.
    #[error("script step {step} is {found}, not a step for {operation}")]
    Mismatch {
        /// The zero-based index of the step in the script.
        step: usize,
        /// The operation that was attempted.
        operation: Operation,
        /// The kind of the step found.
        found: StepKind,
    },
    /// A JSONL replay fixture line is not one typed provider step.
    #[error("script replay line {line}: {detail}")]
    ReplayFormat {
        /// The one-based line containing the invalid record.
        line: usize,
        /// A fixed message that never includes fixture values.
        detail: &'static str,
    },
    /// The configured replay fixture could not be read.
    #[error("script fixture {path} could not be read")]
    FixtureRead {
        /// The absolute fixture path.
        path: std::path::PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayStepWire {
    kind: String,
    #[serde(default)]
    events: Option<Vec<RawJson>>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    outcome: Option<RawJson>,
}

#[derive(Deserialize)]
struct ReplayEventTag {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayTextWire {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayCallStartWire {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayArgsDeltaWire {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    fragment: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayPayloadWire {
    family: Family,
    model: Box<str>,
    item: RawJson,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayReplayWire {
    #[serde(rename = "type")]
    kind: String,
    payload: ReplayPayloadWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayToolCallsWire {
    #[serde(rename = "type")]
    kind: String,
    calls: Vec<ReplayToolCallWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayToolCallWire {
    id: String,
    name: String,
    args: ReplayToolArgsWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayToolArgsWire {
    kind: String,
    #[serde(default)]
    value: Option<RawJson>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayUsageWire {
    #[serde(rename = "type")]
    kind: String,
    usage: Usage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayStopWire {
    #[serde(rename = "type")]
    kind: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayCompactWire {
    kind: String,
    #[serde(default)]
    family: Option<Family>,
    #[serde(default)]
    model: Option<Box<str>>,
    #[serde(default)]
    items: Option<Vec<RawJson>>,
}

fn replay_error(line: usize, detail: &'static str) -> ScriptError {
    ScriptError::ReplayFormat { line, detail }
}

fn decode_replay_step(wire: ReplayStepWire, line: usize) -> Result<ScriptStep, ScriptError> {
    match wire.kind.as_str() {
        "events" if wire.message.is_none() && wire.usage.is_none() && wire.outcome.is_none() => {
            let events = wire
                .events
                .ok_or_else(|| replay_error(line, "events is required"))?;
            let events = events
                .iter()
                .map(|event| decode_replay_event(event, line))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ScriptStep::Events(events))
        }
        "fail" if wire.events.is_none() && wire.usage.is_none() && wire.outcome.is_none() => {
            let message = wire
                .message
                .ok_or_else(|| replay_error(line, "message is required"))?;
            Ok(ScriptStep::Fail(ProviderError::InvalidRequest { message }))
        }
        "usage" if wire.events.is_none() && wire.message.is_none() && wire.outcome.is_none() => {
            let usage = wire
                .usage
                .ok_or_else(|| replay_error(line, "usage is required"))?;
            Ok(ScriptStep::Usage(usage))
        }
        "compact" if wire.events.is_none() && wire.message.is_none() && wire.usage.is_none() => {
            let outcome = wire
                .outcome
                .ok_or_else(|| replay_error(line, "outcome is required"))?
                .decode_as::<ReplayCompactWire>()
                .map_err(|_| replay_error(line, "invalid compact outcome"))?;
            let outcome = match (
                outcome.kind.as_str(),
                outcome.family,
                outcome.model,
                outcome.items,
            ) {
                ("unsupported", None, None, None) => CompactOutcome::Unsupported,
                ("compacted", Some(family), Some(model), Some(items)) => {
                    CompactOutcome::Compacted(crate::compact::CompactedHistory {
                        family,
                        model,
                        items,
                    })
                }
                _ => return Err(replay_error(line, "invalid compact outcome")),
            };
            Ok(ScriptStep::Compact(outcome))
        }
        _ => Err(replay_error(line, "unknown or malformed step")),
    }
}

fn decode_replay_event(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let tag = raw
        .decode_as::<ReplayEventTag>()
        .map_err(|_| replay_error(line, "invalid event"))?;
    match tag.kind.as_str() {
        "text_delta" => decode_text_delta(raw, line, "text_delta"),
        "reasoning_delta" => decode_text_delta(raw, line, "reasoning_delta"),
        "tool_call_started" => decode_call_started(raw, line),
        "tool_args_delta" => decode_args_delta(raw, line),
        "replay" => decode_replay_payload(raw, line),
        "tool_calls_done" => decode_calls_done(raw, line),
        "usage" => decode_usage_event(raw, line),
        "stop" => decode_stop_event(raw, line),
        _ => Err(replay_error(line, "unknown event type")),
    }
}

fn decode_text_delta(raw: &RawJson, line: usize, kind: &str) -> Result<StreamEvent, ScriptError> {
    let detail = if kind == "text_delta" {
        "invalid text delta"
    } else {
        "invalid reasoning delta"
    };
    let wire = raw
        .decode_as::<ReplayTextWire>()
        .map_err(|_| replay_error(line, detail))?;
    if wire.kind.as_str() != kind {
        return Err(replay_error(line, detail));
    }
    let event = if kind == "text_delta" {
        StreamEvent::TextDelta { text: wire.text }
    } else {
        StreamEvent::ReasoningDelta { text: wire.text }
    };
    Ok(event)
}

fn decode_call_started(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayCallStartWire>()
        .map_err(|_| replay_error(line, "invalid tool call start"))?;
    if wire.kind.as_str() != "tool_call_started" {
        return Err(replay_error(line, "invalid tool call start"));
    }
    Ok(StreamEvent::ToolCallStarted {
        id: wire.id,
        name: wire.name,
    })
}

fn decode_args_delta(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayArgsDeltaWire>()
        .map_err(|_| replay_error(line, "invalid tool argument delta"))?;
    if wire.kind.as_str() != "tool_args_delta" {
        return Err(replay_error(line, "invalid tool argument delta"));
    }
    Ok(StreamEvent::ToolArgsDelta {
        id: wire.id,
        fragment: wire.fragment.into_bytes(),
    })
}

fn decode_replay_payload(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayReplayWire>()
        .map_err(|_| replay_error(line, "invalid replay payload"))?;
    if wire.kind.as_str() != "replay" {
        return Err(replay_error(line, "invalid replay payload"));
    }
    Ok(StreamEvent::Replay {
        payload: crate::stream::ReplayPayload {
            family: wire.payload.family,
            model: wire.payload.model,
            item: wire.payload.item,
        },
    })
}

fn decode_calls_done(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayToolCallsWire>()
        .map_err(|_| replay_error(line, "invalid completed tool calls"))?;
    if wire.kind.as_str() != "tool_calls_done" {
        return Err(replay_error(line, "invalid completed tool calls"));
    }
    let calls = wire
        .calls
        .into_iter()
        .map(|call| decode_replay_tool_call(call, line))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StreamEvent::ToolCallsDone { calls })
}

fn decode_usage_event(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayUsageWire>()
        .map_err(|_| replay_error(line, "invalid usage event"))?;
    if wire.kind.as_str() != "usage" {
        return Err(replay_error(line, "invalid usage event"));
    }
    Ok(StreamEvent::Usage { usage: wire.usage })
}

fn decode_stop_event(raw: &RawJson, line: usize) -> Result<StreamEvent, ScriptError> {
    let wire = raw
        .decode_as::<ReplayStopWire>()
        .map_err(|_| replay_error(line, "invalid stop event"))?;
    if wire.kind.as_str() != "stop" {
        return Err(replay_error(line, "invalid stop event"));
    }
    let reason = match wire.reason.as_str() {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        "paused" => StopReason::Paused,
        _ => StopReason::Other(wire.reason),
    };
    Ok(StreamEvent::Stop { reason })
}

fn decode_replay_tool_call(wire: ReplayToolCallWire, line: usize) -> Result<ToolCall, ScriptError> {
    let args = match (wire.args.kind.as_str(), wire.args.value, wire.args.message) {
        ("parsed", Some(value), None) => ToolArgs::Parsed(value),
        ("invalid", None, Some(message)) => ToolArgs::Invalid { message },
        ("truncated", None, None) => ToolArgs::Truncated,
        _ => return Err(replay_error(line, "invalid tool arguments")),
    };
    Ok(ToolCall {
        id: wire.id,
        name: wire.name,
        args,
    })
}

/// A queued step after the build-time grammar check.
#[derive(Debug)]
enum Entry {
    /// A stream; `failure` is the error terminal of a stream without `Stop`.
    Stream {
        events: Vec<StreamEvent>,
        failure: Option<ProviderError>,
    },
    Fail(ProviderError),
    Usage(Usage),
    Compact(CompactOutcome),
}

impl Entry {
    const fn kind(&self) -> StepKind {
        match self {
            Self::Stream { .. } => StepKind::Events,
            Self::Fail(_) => StepKind::Fail,
            Self::Usage(_) => StepKind::Usage,
            Self::Compact(_) => StepKind::Compact,
        }
    }
}

#[derive(Debug)]
struct Queued {
    /// The index of the originating step in the script.
    step: usize,
    entry: Entry,
}

/// A deterministic provider that serves its steps in order.
///
/// It needs no credential, network, or clock. Hooks, notices, and the turn
/// cancellation token do not change what it serves. Clones share one step
/// queue, so every provider built from the same fixture continues the same
/// script instead of restarting it.
#[derive(Clone, Debug)]
pub struct Script {
    queue: Arc<Mutex<VecDeque<Queued>>>,
    /// Streams that ended in an error or were dropped before their terminal.
    aborted: Arc<AtomicUsize>,
}

impl Script {
    /// Builds a script, checking every events step against the stream
    /// grammar.
    ///
    /// # Errors
    /// Returns [`ScriptError::Grammar`] naming the first events step that is
    /// out of order, has an event after `Stop`, or ends before `Stop` without
    /// a fail step right after it.
    pub fn new(steps: Vec<ScriptStep>) -> Result<Self, ScriptError> {
        let mut queue = VecDeque::with_capacity(steps.len());
        let mut steps = steps.into_iter().enumerate().peekable();
        while let Some((step, item)) = steps.next() {
            let entry = match item {
                ScriptStep::Events(events) => {
                    let ending =
                        shape(&events).map_err(|detail| ScriptError::Grammar { step, detail })?;
                    let failure = match ending {
                        Shape::Complete => None,
                        Shape::Prefix => Some(bound_failure(&mut steps, step)?),
                    };
                    Entry::Stream { events, failure }
                }
                ScriptStep::Fail(error) => Entry::Fail(error),
                ScriptStep::Usage(usage) => Entry::Usage(usage),
                ScriptStep::Compact(outcome) => Entry::Compact(outcome),
            };
            queue.push_back(Queued { step, entry });
        }
        Ok(Self {
            queue: Arc::new(Mutex::new(queue)),
            aborted: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Decodes one JSON object per nonblank line into a scripted provider.
    ///
    /// Each line has one `kind`: `events`, `fail`, `usage`, or `compact`.
    /// Event records use `type` names in snake case; raw replay and tool-arg
    /// values remain raw JSON. A `fail` record becomes an `InvalidRequest`.
    ///
    /// # Errors
    /// Returns [`ScriptError::ReplayFormat`] for invalid UTF-8 or any line
    /// that is not a typed record, or [`ScriptError::Grammar`] when decoded
    /// events violate the terminal grammar.
    pub fn from_replay(bytes: &[u8]) -> Result<Self, ScriptError> {
        let text = std::str::from_utf8(bytes).map_err(|_| ScriptError::ReplayFormat {
            line: 1,
            detail: "fixture is not UTF-8",
        })?;
        let mut steps = Vec::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let step = sonic_rs::from_str::<ReplayStepWire>(line).map_err(|_| {
                ScriptError::ReplayFormat {
                    line: index + 1,
                    detail: "expected one typed JSON step",
                }
            })?;
            steps.push(decode_replay_step(step, index + 1)?);
        }
        Self::new(steps)
    }

    /// Loads one replay fixture file into a scripted provider.
    ///
    /// The path is used as given; resolving a configured relative fixture
    /// against the data directory is the provider set's job. The file
    /// content follows [`Script::from_replay`].
    ///
    /// # Errors
    /// Returns [`ScriptError::FixtureRead`] naming the path when the file
    /// cannot be read, or the [`Script::from_replay`] error for a malformed
    /// fixture.
    pub fn from_replay_file(path: &std::path::Path) -> Result<Self, ScriptError> {
        let bytes = std::fs::read(path).map_err(|_| ScriptError::FixtureRead {
            path: path.to_path_buf(),
        })?;
        Self::from_replay(&bytes)
    }

    /// The number of operations the script can still serve.
    ///
    /// An events step and the fail step that ends it count as one.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lock().len()
    }

    /// The number of served streams that ended in an error or were dropped
    /// before their terminal; each counts once.
    #[must_use]
    pub fn aborted_streams(&self) -> usize {
        self.aborted.load(Ordering::SeqCst)
    }

    /// Serves the next events step as a stream, or fails with the next fail
    /// step.
    ///
    /// The step is consumed when the call is made, so calls take steps in
    /// the order they are made.
    ///
    /// # Errors
    /// Returns the scripted failure, or [`ProviderError::Script`] when the
    /// script is exhausted or the next step is not for `open`.
    pub(crate) fn open(&self) -> Result<EventStream, ProviderError> {
        let (events, failure) = self.take(Operation::Open, |entry| match entry {
            Entry::Stream { events, failure } => Ok(Ok((events, failure))),
            Entry::Fail(error) => Ok(Err(error)),
            other => Err(other),
        })?;
        let items = events
            .into_iter()
            .map(Ok)
            .chain(failure.map(Err))
            .collect::<Vec<_>>();
        let aborted = Arc::clone(&self.aborted);
        Ok(EventStream::new(stream::iter(items), move || {
            aborted.fetch_add(1, Ordering::SeqCst);
        }))
    }

    /// Serves the next usage step: the scripted token counters.
    ///
    /// This is the scripted provider's own usage read. It is not an account
    /// usage verdict; a scripted provider has no account to check.
    ///
    /// # Errors
    /// Returns the scripted failure, or [`ProviderError::Script`] when the
    /// script is exhausted or the next step is not for `usage`.
    pub(crate) fn usage(&self) -> Result<Usage, ProviderError> {
        self.take(Operation::Usage, |entry| match entry {
            Entry::Usage(usage) => Ok(Ok(usage)),
            Entry::Fail(error) => Ok(Err(error)),
            other => Err(other),
        })
    }

    /// Serves the next compact step.
    ///
    /// # Errors
    /// Returns the scripted failure, or [`ProviderError::Script`] when the
    /// script is exhausted or the next step is not for `compact`.
    pub(crate) fn compact(&self) -> Result<CompactOutcome, ProviderError> {
        self.take(Operation::Compact, |entry| match entry {
            Entry::Compact(outcome) => Ok(Ok(outcome)),
            Entry::Fail(error) => Ok(Err(error)),
            other => Err(other),
        })
    }

    /// Pops the next entry and hands it to `serve`. An entry `serve` gives
    /// back is not for `operation`: it returns to the front of the queue at
    /// its place and the call fails with [`ScriptError::Mismatch`].
    fn take<T>(
        &self,
        operation: Operation,
        serve: impl FnOnce(Entry) -> Result<Result<T, ProviderError>, Entry>,
    ) -> Result<T, ProviderError> {
        let mut queue = self.lock();
        let Some(Queued { step, entry }) = queue.pop_front() else {
            return Err(ProviderError::Script(ScriptError::Exhausted { operation }));
        };
        serve(entry).unwrap_or_else(|entry| {
            let found = entry.kind();
            queue.push_front(Queued { step, entry });
            Err(ProviderError::Script(ScriptError::Mismatch {
                step,
                operation,
                found,
            }))
        })
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Queued>> {
        // No code under the lock can panic, so a poisoned queue is intact.
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Takes the fail step that must follow an events step without `Stop`.
fn bound_failure(
    steps: &mut Peekable<Enumerate<vec::IntoIter<ScriptStep>>>,
    step: usize,
) -> Result<ProviderError, ScriptError> {
    match steps.next_if(|(_, next)| matches!(next, ScriptStep::Fail(_))) {
        Some((_, ScriptStep::Fail(error))) => Ok(error),
        _ => Err(ScriptError::Grammar {
            step,
            detail: "events without stop must be followed by a fail step",
        }),
    }
}

/// How an events step ends.
enum Shape {
    /// It ends with `Stop`.
    Complete,
    /// It is a valid beginning that ends before `Stop`.
    Prefix,
}

/// Checks one events step against the stream grammar.
fn shape(events: &[StreamEvent]) -> Result<Shape, &'static str> {
    #[derive(PartialEq)]
    enum At {
        Body,
        Done,
        Usage,
        Stopped,
    }
    let mut at = At::Body;
    for event in events {
        at = match (at, event) {
            (At::Stopped, _) => return Err("an event follows stop"),
            (
                At::Body,
                StreamEvent::TextDelta { .. }
                | StreamEvent::ReasoningDelta { .. }
                | StreamEvent::ToolCallStarted { .. }
                | StreamEvent::ToolArgsDelta { .. }
                | StreamEvent::Replay { .. },
            ) => At::Body,
            (At::Body, StreamEvent::ToolCallsDone { .. }) => At::Done,
            (At::Body, StreamEvent::Usage { .. }) => return Err("usage before tool calls done"),
            (At::Body, StreamEvent::Stop { .. }) => return Err("stop before tool calls done"),
            (At::Done, StreamEvent::Usage { .. }) => At::Usage,
            (At::Done, StreamEvent::ToolCallsDone { .. }) => {
                return Err("a second tool calls done");
            }
            (At::Done, StreamEvent::Stop { .. }) => return Err("stop before usage"),
            (At::Done, _) => return Err("a delta or replay after tool calls done"),
            (At::Usage, StreamEvent::Stop { .. }) => At::Stopped,
            (At::Usage, StreamEvent::Usage { .. }) => return Err("a second usage"),
            (At::Usage, _) => return Err("an event between usage and stop"),
        };
    }
    Ok(if at == At::Stopped {
        Shape::Complete
    } else {
        Shape::Prefix
    })
}

#[cfg(test)]
mod tests;
