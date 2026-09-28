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

use dal_core::Usage;
use futures::stream;

use crate::{
    compact::CompactOutcome,
    error::ProviderError,
    stream::{EventStream, StreamEvent},
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
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
/// cancellation token do not change what it serves.
#[derive(Debug)]
pub struct Script {
    queue: Mutex<VecDeque<Queued>>,
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
                    let ending = shape(&events).map_err(|detail| ScriptError::Grammar { step, detail })?;
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
            queue: Mutex::new(queue),
            aborted: Arc::new(AtomicUsize::new(0)),
        })
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
mod tests {
    use std::thread;

    use dal_core::{Family, RawJson};
    use futures::executor::block_on;

    use super::*;
    use crate::{
        compact::CompactedHistory,
        stream::{ReplayPayload, StopReason, ToolArgs, ToolCall},
    };

    fn text(text: &str) -> StreamEvent {
        StreamEvent::TextDelta { text: text.into() }
    }

    fn done() -> StreamEvent {
        StreamEvent::ToolCallsDone { calls: Vec::new() }
    }

    fn tokens(input_tokens: u64, output_tokens: u64) -> Usage {
        Usage {
            input_tokens,
            cached_input_tokens: 0,
            output_tokens,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        }
    }

    fn usage_event() -> StreamEvent {
        StreamEvent::Usage {
            usage: tokens(10, 5),
        }
    }

    fn stop() -> StreamEvent {
        StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }
    }

    fn reply(words: &str) -> ScriptStep {
        ScriptStep::Events(vec![text(words), done(), usage_event(), stop()])
    }

    fn drain(stream: &mut EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
        block_on(async {
            let mut seen = Vec::new();
            while let Some(item) = stream.next().await {
                seen.push(item);
            }
            seen
        })
    }

    fn script_error(error: ProviderError) -> ScriptError {
        match error {
            ProviderError::Script(error) => error,
            other => panic!("expected a script error, got {other:?}"),
        }
    }

    #[test]
    fn text_and_tool_turn_is_served_verbatim_through_event_stream() {
        // Argument fragments split a JSON token and keep odd spacing; the
        // assembled call keeps the exact bytes, never re-encoded.
        let args = br#"{"path": "a.rs" ,"n":1.50}"#;
        let replay = ReplayPayload {
            family: Family::Responses,
            model: "gpt-6-luna".into(),
            item: RawJson::parse(r#"{"type":"reasoning","encrypted_content":"x"}"#).unwrap(),
        };
        let turn = vec![
            StreamEvent::ReasoningDelta {
                text: "think".into(),
            },
            text("Hel"),
            text("lo"),
            StreamEvent::Replay { payload: replay },
            StreamEvent::ToolCallStarted {
                id: "call_1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_1".into(),
                fragment: args[..4].to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "call_1".into(),
                fragment: args[4..].to_vec(),
            },
            StreamEvent::ToolCallsDone {
                calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    args: ToolArgs::from_bytes(args),
                }],
            },
            usage_event(),
            StreamEvent::Stop {
                reason: StopReason::ToolUse,
            },
        ];
        let script = Script::new(vec![ScriptStep::Events(turn.clone())]).unwrap();
        let mut stream = script.open().unwrap();
        let seen = drain(&mut stream)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(seen, turn);
        let StreamEvent::ToolCallsDone { calls } = &seen[7] else {
            panic!("tool calls done moved: {seen:?}");
        };
        let ToolArgs::Parsed(raw) = &calls[0].args else {
            panic!("arguments did not parse: {calls:?}");
        };
        assert_eq!(raw.as_str(), r#"{"path": "a.rs" ,"n":1.50}"#);
        drop(stream);
        assert_eq!(script.aborted_streams(), 0);
        assert_eq!(script.remaining(), 0);
        assert_eq!(
            script_error(script.open().unwrap_err()),
            ScriptError::Exhausted {
                operation: Operation::Open,
            }
        );
    }

    #[test]
    fn fail_alone_fails_open_and_fail_after_events_is_the_stream_terminal() {
        let script = Script::new(vec![
            ScriptStep::Fail(ProviderError::Overloaded),
            ScriptStep::Events(vec![text("par"), text("tial")]),
            ScriptStep::Fail(ProviderError::StreamCut),
            reply("after"),
        ])
        .unwrap();
        assert_eq!(script.remaining(), 3);
        assert!(matches!(script.open(), Err(ProviderError::Overloaded)));

        let mut stream = script.open().unwrap();
        let seen = drain(&mut stream);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].as_ref().unwrap(), &text("par"));
        assert_eq!(seen[1].as_ref().unwrap(), &text("tial"));
        assert!(matches!(seen[2], Err(ProviderError::StreamCut)));
        assert_eq!(script.aborted_streams(), 1);
        drop(stream);
        assert_eq!(script.aborted_streams(), 1);

        // The pair was consumed by one open; the next open gets the reply.
        let mut stream = script.open().unwrap();
        assert_eq!(drain(&mut stream).len(), 4);
        assert_eq!(script.remaining(), 0);
    }

    #[test]
    fn empty_events_before_fail_opens_then_fails_without_events() {
        let script = Script::new(vec![
            ScriptStep::Events(Vec::new()),
            ScriptStep::Fail(ProviderError::Overloaded),
        ])
        .unwrap();
        let mut stream = script.open().unwrap();
        let seen = drain(&mut stream);
        assert!(matches!(seen.as_slice(), [Err(ProviderError::Overloaded)]));
    }

    #[test]
    fn usage_steps_serve_counters_or_failures_in_order() {
        let script = Script::new(vec![
            ScriptStep::Usage(tokens(10, 5)),
            ScriptStep::Fail(ProviderError::Overloaded),
            ScriptStep::Usage(tokens(3, 4)),
        ])
        .unwrap();
        assert_eq!(script.usage().unwrap(), tokens(10, 5));
        assert!(matches!(script.usage(), Err(ProviderError::Overloaded)));
        assert_eq!(script.usage().unwrap(), tokens(3, 4));
        assert_eq!(
            script_error(script.usage().unwrap_err()),
            ScriptError::Exhausted {
                operation: Operation::Usage,
            }
        );
    }

    #[test]
    fn compact_steps_return_bound_raw_history_and_unsupported() {
        let item = r#"{"type":"compaction","encrypted_content":"e30=" , "id":"cmp_1"}"#;
        let history = CompactedHistory::new(
            Family::Codex,
            "gpt-6-luna",
            vec![RawJson::parse(item).unwrap()],
        );
        let script = Script::new(vec![
            ScriptStep::Compact(CompactOutcome::Compacted(history.clone())),
            ScriptStep::Compact(CompactOutcome::Unsupported),
            ScriptStep::Fail(ProviderError::CompactionMissing {
                family: Family::Anthropic,
                noun: "block",
            }),
        ])
        .unwrap();
        let CompactOutcome::Compacted(served) = script.compact().unwrap() else {
            panic!("compacted history was not served");
        };
        assert_eq!(served, history);
        let items = served.items_for(Family::Codex, "gpt-6-luna").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].as_str(), item);
        assert!(matches!(
            served.items_for(Family::Codex, "gpt-5.6-luna"),
            Err(ProviderError::CompactionForeign { .. })
        ));
        assert_eq!(script.compact().unwrap(), CompactOutcome::Unsupported);
        assert!(matches!(
            script.compact(),
            Err(ProviderError::CompactionMissing {
                family: Family::Anthropic,
                noun: "block",
            })
        ));
    }

    #[test]
    fn wrong_step_is_a_typed_error_and_stays_in_place() {
        let script = Script::new(vec![reply("hi"), ScriptStep::Usage(tokens(1, 1))]).unwrap();
        let error = script_error(script.usage().unwrap_err());
        assert_eq!(
            error,
            ScriptError::Mismatch {
                step: 0,
                operation: Operation::Usage,
                found: StepKind::Events,
            }
        );
        assert_eq!(error.to_string(), "script step 0 is events, not a step for usage");
        let failure = ProviderError::Script(error);
        assert_eq!(failure.to_string(), "script step 0 is events, not a step for usage");
        assert!(!failure.retryable_by_loop());
        assert!(failure.fix().is_none());
        assert!(matches!(
            dal_core::InferFailure::from(failure),
            dal_core::InferFailure::Fatal { fix: None, .. }
        ));
        assert_eq!(
            script_error(script.compact().unwrap_err()),
            ScriptError::Mismatch {
                step: 0,
                operation: Operation::Compact,
                found: StepKind::Events,
            }
        );
        assert_eq!(script.remaining(), 2);

        let mut stream = script.open().unwrap();
        assert_eq!(drain(&mut stream).len(), 4);
        assert_eq!(
            script_error(script.open().unwrap_err()),
            ScriptError::Mismatch {
                step: 1,
                operation: Operation::Open,
                found: StepKind::Usage,
            }
        );
        assert_eq!(script.usage().unwrap(), tokens(1, 1));
        assert_eq!(
            ScriptError::Exhausted {
                operation: Operation::Compact
            }
            .to_string(),
            "script exhausted: no step left for compact"
        );
    }

    #[test]
    fn grammar_violations_are_rejected_at_build_naming_the_step() {
        let cases: Vec<(Vec<ScriptStep>, usize, &'static str)> = vec![
            (
                vec![ScriptStep::Events(vec![done(), usage_event(), stop(), text("late")])],
                0,
                "an event follows stop",
            ),
            (
                vec![ScriptStep::Events(vec![done(), usage_event(), stop(), stop()])],
                0,
                "an event follows stop",
            ),
            (
                vec![ScriptStep::Events(vec![usage_event(), done(), stop()])],
                0,
                "usage before tool calls done",
            ),
            (
                vec![ScriptStep::Events(vec![text("a"), stop()])],
                0,
                "stop before tool calls done",
            ),
            (
                vec![ScriptStep::Events(vec![done(), done(), usage_event(), stop()])],
                0,
                "a second tool calls done",
            ),
            (
                vec![ScriptStep::Events(vec![done(), stop()])],
                0,
                "stop before usage",
            ),
            (
                vec![ScriptStep::Events(vec![done(), text("a"), usage_event(), stop()])],
                0,
                "a delta or replay after tool calls done",
            ),
            (
                vec![ScriptStep::Events(vec![done(), usage_event(), usage_event(), stop()])],
                0,
                "a second usage",
            ),
            (
                vec![ScriptStep::Events(vec![done(), usage_event(), text("a"), stop()])],
                0,
                "an event between usage and stop",
            ),
            (
                vec![reply("ok"), ScriptStep::Events(vec![text("cut")])],
                1,
                "events without stop must be followed by a fail step",
            ),
            (
                vec![
                    ScriptStep::Events(vec![text("cut")]),
                    ScriptStep::Usage(tokens(1, 1)),
                ],
                0,
                "events without stop must be followed by a fail step",
            ),
        ];
        for (steps, step, detail) in cases {
            assert_eq!(
                Script::new(steps).unwrap_err(),
                ScriptError::Grammar { step, detail },
                "{detail}"
            );
        }
    }

    #[test]
    fn step_indices_count_the_bound_fail_step() {
        let script = Script::new(vec![
            ScriptStep::Events(vec![text("cut")]),
            ScriptStep::Fail(ProviderError::StreamCut),
            ScriptStep::Usage(tokens(1, 1)),
        ])
        .unwrap();
        let mut stream = script.open().unwrap();
        assert_eq!(drain(&mut stream).len(), 2);
        assert_eq!(
            script_error(script.open().unwrap_err()),
            ScriptError::Mismatch {
                step: 2,
                operation: Operation::Open,
                found: StepKind::Usage,
            }
        );
    }

    #[test]
    fn simultaneous_opens_take_each_step_once_in_script_order() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 16;
        let steps = (0..THREADS * PER_THREAD)
            .map(|index| reply(&index.to_string()))
            .collect();
        let script = Arc::new(Script::new(steps).unwrap());
        let workers = (0..THREADS)
            .map(|_| {
                let script = Arc::clone(&script);
                thread::spawn(move || {
                    (0..PER_THREAD)
                        .map(|_| {
                            let mut stream = script.open().unwrap();
                            let first = block_on(stream.next()).unwrap().unwrap();
                            // Each served stream still ends in exactly one Stop.
                            let rest = drain(&mut stream);
                            assert_eq!(rest.len(), 3);
                            assert!(matches!(rest[2], Ok(StreamEvent::Stop { .. })));
                            let StreamEvent::TextDelta { text } = first else {
                                panic!("first event moved: {first:?}");
                            };
                            text.parse::<usize>().unwrap()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut all = Vec::new();
        for worker in workers {
            let taken = worker.join().unwrap();
            // One caller's consecutive opens see steps in script order.
            assert!(taken.windows(2).all(|pair| pair[0] < pair[1]), "{taken:?}");
            all.extend(taken);
        }
        all.sort_unstable();
        assert_eq!(all, (0..THREADS * PER_THREAD).collect::<Vec<_>>());
        assert_eq!(script.remaining(), 0);
        assert_eq!(script.aborted_streams(), 0);
    }

    #[test]
    fn interleaved_calls_are_served_in_call_order() {
        let script = Script::new(vec![
            reply("first"),
            reply("second"),
            ScriptStep::Usage(tokens(2, 2)),
        ])
        .unwrap();
        // Both streams are taken before either is read.
        let mut first = script.open().unwrap();
        let mut second = script.open().unwrap();
        assert_eq!(script.usage().unwrap(), tokens(2, 2));
        assert_eq!(drain(&mut second)[0].as_ref().unwrap(), &text("second"));
        assert_eq!(drain(&mut first)[0].as_ref().unwrap(), &text("first"));
    }

    #[test]
    fn early_drop_cancels_once_and_stop_is_delivered_once() {
        let script = Script::new(vec![reply("dropped"), reply("finished")]).unwrap();

        let mut dropped = script.open().unwrap();
        assert_eq!(
            block_on(dropped.next()).unwrap().unwrap(),
            text("dropped")
        );
        drop(dropped);
        assert_eq!(script.aborted_streams(), 1);

        let mut finished = script.open().unwrap();
        let seen = drain(&mut finished);
        let stops = seen
            .iter()
            .filter(|item| matches!(item, Ok(StreamEvent::Stop { .. })))
            .count();
        assert_eq!(stops, 1);
        assert!(matches!(seen.last(), Some(Ok(StreamEvent::Stop { .. }))));
        assert!(block_on(finished.next()).is_none());
        assert!(block_on(finished.next()).is_none());
        drop(finished);
        assert_eq!(script.aborted_streams(), 1);
    }
}
