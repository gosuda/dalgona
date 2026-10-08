use std::thread;

use dal_core::{Family, RawJson};
use futures::executor::block_on;

use super::*;
use crate::{
    compact::{CompactedHistory, items_for},
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
    let history = CompactedHistory {
        family: Family::Codex,
        model: "gpt-6-luna".into(),
        items: vec![RawJson::parse(item).unwrap()],
    };
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
        panic!("compacted history was not served")
    };
    assert_eq!(served, history);
    let items = items_for(&served, Family::Codex, "gpt-6-luna").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].as_str(), item);
    assert!(matches!(
        items_for(&served, Family::Codex, "gpt-5.6-luna"),
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
    assert_eq!(
        error.to_string(),
        "script step 0 is events, not a step for usage"
    );
    let failure = ProviderError::Script(error);
    assert_eq!(
        failure.to_string(),
        "script step 0 is events, not a step for usage"
    );
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
            vec![ScriptStep::Events(vec![
                done(),
                usage_event(),
                stop(),
                text("late"),
            ])],
            0,
            "an event follows stop",
        ),
        (
            vec![ScriptStep::Events(vec![
                done(),
                usage_event(),
                stop(),
                stop(),
            ])],
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
            vec![ScriptStep::Events(vec![
                done(),
                done(),
                usage_event(),
                stop(),
            ])],
            0,
            "a second tool calls done",
        ),
        (
            vec![ScriptStep::Events(vec![done(), stop()])],
            0,
            "stop before usage",
        ),
        (
            vec![ScriptStep::Events(vec![
                done(),
                text("a"),
                usage_event(),
                stop(),
            ])],
            0,
            "a delta or replay after tool calls done",
        ),
        (
            vec![ScriptStep::Events(vec![
                done(),
                usage_event(),
                usage_event(),
                stop(),
            ])],
            0,
            "a second usage",
        ),
        (
            vec![ScriptStep::Events(vec![
                done(),
                usage_event(),
                text("a"),
                stop(),
            ])],
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
    assert_eq!(block_on(dropped.next()).unwrap().unwrap(), text("dropped"));
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

#[test]
fn replay_fail_with_status_and_family_is_an_http_status_failure() {
    let script = Script::from_replay(
        br#"{"kind":"fail","message":"body too large","status":413,"family":"openai_chat"}"#,
    )
    .expect("fail with status decodes");
    match script.open() {
        Err(ProviderError::Status {
            family,
            status,
            message,
        }) => {
            assert_eq!(family, Family::Chat);
            assert_eq!(status, 413);
            assert_eq!(message, "body too large");
        }
        other => panic!("expected a status failure, got {:?}", other.err()),
    }
}

#[test]
fn replay_fail_status_and_family_must_come_together() {
    for line in [
        r#"{"kind":"fail","message":"x","status":413}"#,
        r#"{"kind":"fail","message":"x","family":"openai_chat"}"#,
        r#"{"kind":"usage","usage":{"input_tokens":1,"output_tokens":1},"status":413,"family":"openai_chat"}"#,
    ] {
        assert!(
            matches!(
                Script::from_replay(line.as_bytes()),
                Err(ScriptError::ReplayFormat { line: 1, .. })
            ),
            "{line} must be rejected"
        );
    }
}
