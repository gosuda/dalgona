use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use dal_core::{InferFailure, RawJson, Usage};
use futures::{FutureExt, channel::mpsc, executor::block_on};

use super::*;
use crate::stream::ToolArgs;

type Item = Result<StreamEvent, ProviderError>;

/// Room for the longest burst a test pushes before it reads.
const SCRIPT_ROOM: usize = 32;

#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<Vec<HostToolResult>>>,
    aborts: AtomicUsize,
    refuse: Mutex<Option<RunLostReason>>,
}

/// A transport that keeps what the run sends and counts aborts. It honors the
/// `RunTransport` contract: it refuses a send only while the test says the
/// connection is gone, and a send after an abort fails the test.
struct Loopback(Arc<Wire>);

impl RunTransport for Loopback {
    fn send(&mut self, results: Vec<HostToolResult>) -> Result<(), RunLostReason> {
        assert_eq!(self.0.aborts.load(Ordering::SeqCst), 0, "send after abort");
        if let Some(reason) = self.0.refuse.lock().unwrap().clone() {
            return Err(reason);
        }
        self.0.sent.lock().unwrap().push(results);
        Ok(())
    }

    fn abort(&mut self) {
        self.0.aborts.fetch_add(1, Ordering::SeqCst);
    }
}

/// The server end of a loopback run: it feeds decoded events and sees what
/// the run did to the connection.
struct Server {
    events: mpsc::Sender<Item>,
    wire: Arc<Wire>,
}

impl Server {
    fn push(&mut self, events: impl IntoIterator<Item = StreamEvent>) {
        for event in events {
            self.events.try_send(Ok(event)).unwrap();
        }
    }

    fn fail(&mut self, error: ProviderError) {
        self.events.try_send(Err(error)).unwrap();
    }

    /// Ends the event source without a terminal.
    fn hang_up(&mut self) {
        self.events.close_channel();
    }

    fn refuse_sends(&self, reason: Option<RunLostReason>) {
        *self.wire.refuse.lock().unwrap() = reason;
    }

    fn sent(&self) -> Vec<Vec<HostToolResult>> {
        self.wire.sent.lock().unwrap().clone()
    }

    fn aborts(&self) -> usize {
        self.wire.aborts.load(Ordering::SeqCst)
    }

    /// Whether the run still holds the event source: the peer of a released
    /// run sees the connection closed.
    fn connected(&self) -> bool {
        !self.events.is_closed()
    }
}

fn loopback() -> (ProviderRun, Server) {
    let (events, inbox) = mpsc::channel(SCRIPT_ROOM);
    let wire = Arc::new(Wire::default());
    let run = ProviderRun::new(Family::Chat, inbox, Loopback(Arc::clone(&wire)));
    (run, Server { events, wire })
}

fn text(text: &str) -> StreamEvent {
    StreamEvent::TextDelta { text: text.into() }
}

fn zero_usage() -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        },
    }
}

fn stop(reason: StopReason) -> StreamEvent {
    StreamEvent::Stop { reason }
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        args: ToolArgs::Parsed(RawJson::parse("{}").unwrap()),
    }
}

fn calls_done(ids: &[&str]) -> StreamEvent {
    StreamEvent::ToolCallsDone {
        calls: ids.iter().map(|id| tool_call(id, "read")).collect(),
    }
}

/// A step that ends in tool use for `ids`, every call named `read`.
fn tool_step(ids: &[&str]) -> Vec<StreamEvent> {
    let mut events = vec![text("calling")];
    for id in ids {
        events.push(StreamEvent::ToolCallStarted {
            id: (*id).into(),
            name: "read".into(),
        });
        events.push(StreamEvent::ToolArgsDelta {
            id: (*id).into(),
            fragment: b"{}".to_vec(),
        });
    }
    events.extend([calls_done(ids), zero_usage(), stop(StopReason::ToolUse)]);
    events
}

fn final_step(reason: StopReason) -> Vec<StreamEvent> {
    vec![text("done"), calls_done(&[]), zero_usage(), stop(reason)]
}

fn answer(id: &str) -> HostToolResult {
    HostToolResult {
        call: CallId::new(id),
        name: "read".into(),
        is_error: false,
        parts: vec![Part::Text {
            text: format!("result {id}").into(),
        }],
    }
}

fn awaiting(ids: &[&str]) -> RunState {
    RunState::Awaiting(ids.iter().map(|id| CallId::new(*id)).collect())
}

/// Reads one step up to and including its terminal, without polling again
/// after a `Stop`.
fn read_step(stream: &mut EventStream) -> Vec<Item> {
    block_on(async {
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            let ended = matches!(item, Err(_) | Ok(StreamEvent::Stop { .. }));
            items.push(item);
            if ended {
                break;
            }
        }
        items
    })
}

fn events_of(items: Vec<Item>) -> Vec<StreamEvent> {
    items.into_iter().map(Result::unwrap).collect()
}

/// Streams the first step of `ids`, hands the answers over and returns the
/// run waiting for its second step.
fn handed_off(ids: &[&str]) -> (ProviderRun, Server) {
    let (mut run, mut server) = loopback();
    server.push(tool_step(ids));
    read_step(&mut run.next_step().unwrap());
    run.respond(ids.iter().map(|id| answer(id)).collect())
        .unwrap();
    (run, server)
}

/// The first step streams `partial`, then fails with `failure` or, when
/// `None`, ends without a terminal.
fn failing_first_step(
    failure: Option<ProviderError>,
) -> (Vec<StreamEvent>, ProviderError, ProviderRun, Server) {
    let (mut run, mut server) = loopback();
    let mut step = run.next_step().unwrap();
    server.push([text("partial")]);
    match failure {
        Some(error) => server.fail(error),
        None => server.hang_up(),
    }
    let mut items = read_step(&mut step);
    let error = items.pop().unwrap().unwrap_err();
    (events_of(items), error, run, server)
}

/// The same failure on the second step, after one tool handoff.
fn failing_second_step(
    failure: Option<ProviderError>,
) -> (Vec<StreamEvent>, ProviderError, ProviderRun, Server) {
    let (mut run, mut server) = handed_off(&["a"]);
    let mut step = run.next_step().unwrap();
    server.push([text("partial")]);
    match failure {
        Some(error) => server.fail(error),
        None => server.hang_up(),
    }
    let mut items = read_step(&mut step);
    let error = items.pop().unwrap().unwrap_err();
    (events_of(items), error, run, server)
}

fn is_misuse(result: &Result<impl Sized, ProviderError>) -> bool {
    matches!(result, Err(ProviderError::InvalidRequest { .. }))
}

fn is_lost(result: &Result<impl Sized, ProviderError>) -> bool {
    matches!(result, Err(ProviderError::RunLost { .. }))
}

#[test]
fn tool_use_keeps_the_run_alive_across_a_host_exchange() {
    let (mut run, mut server) = loopback();
    assert_eq!(run.state(), RunState::Streaming);

    server.push(tool_step(&["a", "b"]));
    let mut first = run.next_step().unwrap();
    assert_eq!(events_of(read_step(&mut first)), tool_step(&["a", "b"]));
    // The state changed before the consumer saw the Stop.
    assert_eq!(run.state(), awaiting(&["a", "b"]));

    // A clean Stop never runs the stream's cancellation action, so dropping
    // the finished stream leaves the run and its connection alone.
    drop(first);
    assert_eq!(run.state(), awaiting(&["a", "b"]));
    assert_eq!((server.aborts(), server.connected()), (0, true));

    let results = vec![
        answer("b"),
        HostToolResult {
            call: CallId::new("a"),
            name: "read".into(),
            is_error: true,
            parts: vec![
                Part::Text {
                    text: "partial output".into(),
                },
                Part::Image {
                    mime: "image/png".into(),
                    bytes: vec![1, 2, 3].into(),
                },
            ],
        },
    ];
    run.respond(results.clone()).unwrap();
    assert_eq!(run.state(), RunState::Streaming);
    // Exactly as given, in the caller's order.
    assert_eq!(server.sent(), vec![results]);
    assert_eq!(server.aborts(), 0);

    server.push(final_step(StopReason::EndTurn));
    let mut second = run.next_step().unwrap();
    assert_eq!(
        events_of(read_step(&mut second)),
        final_step(StopReason::EndTurn)
    );
    assert_eq!(run.state(), RunState::Closed);
    assert_eq!((server.aborts(), server.connected()), (1, false));
    assert!(is_misuse(&run.next_step()));
    assert!(is_misuse(&run.respond(vec![answer("a")])));
    drop(second);
    drop(run);
    assert_eq!(server.aborts(), 1);
}

#[test]
fn every_terminal_stop_closes_the_run_even_with_calls_reported() {
    let calls_but_max_tokens = {
        let mut events = tool_step(&["a"]);
        *events.last_mut().unwrap() = stop(StopReason::MaxTokens);
        events
    };
    let mut scripts: Vec<Vec<StreamEvent>> = [
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::Refusal,
        StopReason::Paused,
        StopReason::Other(String::from("content_filter")),
    ]
    .into_iter()
    .map(final_step)
    .collect();
    scripts.push(calls_but_max_tokens);
    for script in scripts {
        let (mut run, mut server) = loopback();
        server.push(script.clone());
        let mut step = run.next_step().unwrap();
        assert_eq!(events_of(read_step(&mut step)), script);
        assert_eq!(run.state(), RunState::Closed, "{script:?}");
        assert_eq!((server.aborts(), server.connected()), (1, false));
        assert!(is_misuse(&run.respond(vec![answer("a")])));
        drop(step);
        drop(run);
        assert_eq!(server.aborts(), 1);
    }
}

#[test]
fn a_stalled_step_yields_nothing_until_the_transport_does() {
    let (mut run, mut server) = loopback();
    let mut step = run.next_step().unwrap();
    assert!(step.next().now_or_never().is_none());
    assert_eq!(run.state(), RunState::Streaming);
    server.push([text("x")]);
    assert_eq!(
        step.next().now_or_never().unwrap().unwrap().unwrap(),
        text("x")
    );
    assert!(step.next().now_or_never().is_none());
}

#[test]
fn dropping_a_step_before_its_terminal_releases_the_run_and_aborts_once() {
    // Never polled, after one event, and after every event but the Stop.
    for read in [0, 1, 5] {
        let (mut run, mut server) = loopback();
        server.push(tool_step(&["a"]));
        let mut step = run.next_step().unwrap();
        for _ in 0..read {
            block_on(step.next()).unwrap().unwrap();
        }
        assert_eq!(run.state(), RunState::Streaming, "{read}");
        drop(step);
        assert_eq!(run.state(), RunState::Released, "{read}");
        assert_eq!((server.aborts(), server.connected()), (1, false), "{read}");
        // Released is not Lost, and nothing starts a run by itself.
        assert!(is_misuse(&run.next_step()), "{read}");
        assert!(is_misuse(&run.respond(vec![answer("a")])), "{read}");
        drop(run);
        assert_eq!(server.aborts(), 1, "{read}");
    }
}

#[test]
fn dropping_the_run_aborts_it_whether_awaiting_or_streaming() {
    let (run, server) = handed_off(&["a"]);
    drop(run);
    assert_eq!((server.aborts(), server.connected()), (1, false));

    let (mut run, mut server) = loopback();
    server.push(tool_step(&["a"]));
    read_step(&mut run.next_step().unwrap());
    assert_eq!(run.state(), awaiting(&["a"]));
    drop(run);
    assert_eq!((server.aborts(), server.connected()), (1, false));

    // A live step stream outlives its run: it ends with a typed fatal error,
    // not a retryable cut, and the cancellation it then runs aborts nothing
    // twice.
    let (mut run, mut server) = loopback();
    let mut step = run.next_step().unwrap();
    server.push([text("a")]);
    assert_eq!(block_on(step.next()).unwrap().unwrap(), text("a"));
    drop(run);
    assert_eq!(server.aborts(), 1);
    let error = block_on(step.next()).unwrap().unwrap_err();
    assert!(matches!(
        error,
        ProviderError::RunLost {
            reason: RunLostReason::Transport(_),
            ..
        }
    ));
    assert!(!error.retryable_by_loop());
    assert!(block_on(step.next()).is_none());
    drop(step);
    assert_eq!(server.aborts(), 1);
}

#[test]
fn respond_demands_exactly_the_pending_calls_and_sends_nothing_otherwise() {
    let renamed = HostToolResult {
        name: "write".into(),
        ..answer("a")
    };
    let cases = [
        ("empty", vec![]),
        ("missing", vec![answer("a")]),
        ("extra", vec![answer("a"), answer("b"), answer("c")]),
        ("wrong id", vec![answer("a"), answer("x")]),
        ("one call twice", vec![answer("a"), answer("a")]),
        (
            "duplicate beside the full set",
            vec![answer("a"), answer("b"), answer("a")],
        ),
        ("wrong name", vec![renamed, answer("b")]),
    ];
    for (label, results) in cases {
        let (mut run, mut server) = loopback();
        server.push(tool_step(&["a", "b"]));
        read_step(&mut run.next_step().unwrap());
        let error = run.respond(results).unwrap_err();
        assert!(
            matches!(
                error,
                ProviderError::RunLost {
                    reason: RunLostReason::ResultsMismatch,
                    ..
                }
            ),
            "{label}: {error}"
        );
        assert!(server.sent().is_empty(), "{label}");
        assert_eq!(
            run.state(),
            RunState::Lost(RunLostReason::ResultsMismatch),
            "{label}"
        );
        assert_eq!((server.aborts(), server.connected()), (1, false), "{label}");
        // A lost run is final: the right answer is refused and no run starts.
        assert!(
            is_lost(&run.respond(vec![answer("a"), answer("b")])),
            "{label}"
        );
        assert!(is_lost(&run.next_step()), "{label}");
        assert!(server.sent().is_empty(), "{label}");
    }
}

#[test]
fn a_failed_send_loses_the_run_and_nothing_replaces_it() {
    let reasons = [
        RunLostReason::Transport(String::from("connection reset")),
        RunLostReason::BufferExceeded,
        RunLostReason::ServerClosed,
    ];
    for reason in reasons {
        let (mut run, mut server) = loopback();
        server.push(tool_step(&["a"]));
        read_step(&mut run.next_step().unwrap());
        server.refuse_sends(Some(reason.clone()));
        let error = run.respond(vec![answer("a")]).unwrap_err();
        assert!(
            matches!(&error, ProviderError::RunLost { reason: lost, .. } if *lost == reason),
            "{error}"
        );
        assert_eq!(run.state(), RunState::Lost(reason.clone()));
        assert_eq!((server.aborts(), server.sent().len()), (1, 0));
        // The connection coming back does not revive the run.
        server.refuse_sends(None);
        assert!(is_lost(&run.respond(vec![answer("a")])));
        assert!(is_lost(&run.next_step()));
        assert_eq!(server.sent(), [] as [Vec<HostToolResult>; 0]);
    }
}

#[test]
fn misuse_is_a_typed_error_that_leaves_the_run_intact() {
    let (mut run, mut server) = loopback();
    let mut first = run.next_step().unwrap();
    // One live step stream, and nothing to answer while a step is in flight.
    assert!(is_misuse(&run.next_step()));
    assert!(is_misuse(&run.respond(vec![answer("a")])));
    assert_eq!(run.state(), RunState::Streaming);

    server.push(tool_step(&["a"]));
    read_step(&mut first);
    // The results come before the next step, and a refused call changes nothing.
    assert!(is_misuse(&run.next_step()));
    assert_eq!(run.state(), awaiting(&["a"]));
    run.respond(vec![answer("a")]).unwrap();
    // One answer per step.
    assert!(is_misuse(&run.respond(vec![answer("a")])));
    assert_eq!(server.sent().len(), 1);

    server.push(final_step(StopReason::EndTurn));
    let mut second = run.next_step().unwrap();
    assert_eq!(
        events_of(read_step(&mut second)),
        final_step(StopReason::EndTurn)
    );
    assert_eq!(run.state(), RunState::Closed);
}

#[test]
fn events_decoded_behind_a_tool_use_stop_belong_to_the_next_step() {
    let (mut run, mut server) = loopback();
    // The transport had already decoded the next step's first event.
    server.push(tool_step(&["a"]));
    server.push([text("queued")]);
    let mut first = run.next_step().unwrap();
    assert_eq!(events_of(read_step(&mut first)), tool_step(&["a"]));
    run.respond(vec![answer("a")]).unwrap();
    let mut second = run.next_step().unwrap();
    server.push([calls_done(&[]), zero_usage(), stop(StopReason::EndTurn)]);
    // The finished stream, kept alive across the exchange, meets the
    // stream's late-event check with a fused source: no panic, and neither
    // the queued event nor the next step's events are taken from the next
    // stream. Dropping it releases nothing.
    assert!(block_on(first.next()).is_none());
    drop(first);
    assert_eq!(run.state(), RunState::Streaming);
    assert_eq!(server.aborts(), 0);
    assert_eq!(
        events_of(read_step(&mut second)),
        vec![
            text("queued"),
            calls_done(&[]),
            zero_usage(),
            stop(StopReason::EndTurn)
        ]
    );
    assert_eq!(run.state(), RunState::Closed);
}

#[test]
fn after_a_handoff_every_failure_is_a_fatal_run_loss_and_nothing_is_invented() {
    let transport = |error: &ProviderError| RunLostReason::Transport(error.to_string());
    let failures = [
        ProviderError::Transport {
            family: Family::Chat,
            reason: String::from("reset"),
        },
        ProviderError::Overloaded,
        ProviderError::ContextOverflow {
            family: Family::Chat,
            code: String::from("context_length_exceeded"),
            message: String::from("too long"),
        },
    ];
    let mut cases: Vec<(Option<ProviderError>, RunLostReason)> = failures
        .into_iter()
        .map(|error| {
            let reason = transport(&error);
            (Some(error), reason)
        })
        .collect();
    cases.extend([
        (Some(ProviderError::StreamCut), RunLostReason::ServerClosed),
        (
            Some(ProviderError::WsClosed { code: None }),
            RunLostReason::ServerClosed,
        ),
        (
            Some(ProviderError::Protocol {
                family: Family::Chat,
                detail: String::from("bad frame"),
            }),
            RunLostReason::Protocol(String::from("bad frame")),
        ),
        (None, RunLostReason::ServerClosed),
        (
            Some(ProviderError::RunLost {
                family: Family::Chat,
                reason: RunLostReason::Authority("web_fetch".into()),
            }),
            RunLostReason::Authority("web_fetch".into()),
        ),
    ]);
    for (failure, expected) in cases {
        let (events, error, mut run, server) = failing_second_step(failure);
        // Only what streamed: no Usage and no Stop appear on a failure.
        assert_eq!(events, vec![text("partial")], "{expected}");
        assert!(
            matches!(&error, ProviderError::RunLost { family: Family::Chat, reason } if *reason == expected),
            "{expected}: {error}"
        );
        assert!(!error.retryable_by_loop());
        assert_eq!(run.state(), RunState::Lost(expected.clone()));
        assert_eq!((server.aborts(), server.connected()), (1, false));
        assert!(is_lost(&run.respond(vec![answer("a")])));
        assert!(is_lost(&run.next_step()));
        assert!(matches!(
            InferFailure::from(error),
            InferFailure::Fatal { .. }
        ));
    }
}

#[test]
fn before_the_first_handoff_a_failure_keeps_its_own_class_and_releases_the_run() {
    // A transient failure stays replayable: a fresh run is equivalent.
    let (events, error, mut run, server) = failing_first_step(Some(ProviderError::Overloaded));
    assert_eq!(events, vec![text("partial")]);
    assert!(matches!(error, ProviderError::Overloaded));
    assert!(error.retryable_by_loop());
    assert_eq!(run.state(), RunState::Released);
    assert_eq!((server.aborts(), server.connected()), (1, false));
    assert!(is_misuse(&run.next_step()));

    // A typed overflow still reaches the compaction retry.
    let overflow = ProviderError::ContextOverflow {
        family: Family::Chat,
        code: String::from("context_length_exceeded"),
        message: String::from("too long"),
    };
    let (_, error, run, server) = failing_first_step(Some(overflow));
    assert!(matches!(
        InferFailure::from(error),
        InferFailure::Overflow { .. }
    ));
    assert_eq!(run.state(), RunState::Released);
    assert_eq!(server.aborts(), 1);

    // A source that ends without a terminal is the stream's own cut.
    let (events, error, run, server) = failing_first_step(None);
    assert_eq!(events, vec![text("partial")]);
    assert!(matches!(error, ProviderError::StreamCut));
    assert_eq!(run.state(), RunState::Released);
    assert_eq!(server.aborts(), 1);

    // An adapter's typed loss is final even before the first handoff: the
    // server may already have acted.
    let authority = ProviderError::RunLost {
        family: Family::Chat,
        reason: RunLostReason::Authority("connect_scm".into()),
    };
    let (_, error, run, server) = failing_first_step(Some(authority));
    assert!(matches!(
        error,
        ProviderError::RunLost {
            reason: RunLostReason::Authority(_),
            ..
        }
    ));
    assert_eq!(
        run.state(),
        RunState::Lost(RunLostReason::Authority("connect_scm".into()))
    );
    assert_eq!(server.aborts(), 1);
}

#[test]
fn a_step_that_breaks_the_call_grammar_loses_the_run_instead_of_awaiting() {
    let cases = [
        (
            "tool use naming no call",
            vec![calls_done(&[]), zero_usage(), stop(StopReason::ToolUse)],
        ),
        (
            "tool use without ToolCallsDone",
            vec![zero_usage(), stop(StopReason::ToolUse)],
        ),
        (
            "tool calls reported twice",
            vec![
                calls_done(&["a"]),
                calls_done(&["a"]),
                zero_usage(),
                stop(StopReason::ToolUse),
            ],
        ),
        (
            "one call id twice",
            vec![
                calls_done(&["a", "a"]),
                zero_usage(),
                stop(StopReason::ToolUse),
            ],
        ),
    ];
    for (label, script) in cases {
        let (mut run, mut server) = loopback();
        server.push(script);
        let mut step = run.next_step().unwrap();
        let mut items = read_step(&mut step);
        let error = items.pop().unwrap().unwrap_err();
        assert!(
            matches!(
                error,
                ProviderError::RunLost {
                    reason: RunLostReason::Protocol(_),
                    ..
                }
            ),
            "{label}: {error}"
        );
        // The Stop is never delivered for a step the run cannot answer.
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, Ok(StreamEvent::Stop { .. }))),
            "{label}"
        );
        assert!(
            matches!(run.state(), RunState::Lost(RunLostReason::Protocol(_))),
            "{label}"
        );
        assert_eq!((server.aborts(), server.connected()), (1, false), "{label}");
    }
}

#[test]
fn a_step_stream_runs_on_another_thread_while_the_run_stays_here() {
    let (mut run, mut server) = loopback();
    let mut step = run.next_step().unwrap();
    let reader = thread::spawn(move || {
        let items = read_step(&mut step);
        (items, step)
    });
    server.push(tool_step(&["a"]));
    let (items, _finished) = reader.join().unwrap();
    assert_eq!(events_of(items), tool_step(&["a"]));
    assert_eq!(run.state(), awaiting(&["a"]));
    run.respond(vec![answer("a")]).unwrap();
    assert_eq!(server.sent(), vec![vec![answer("a")]]);
}
