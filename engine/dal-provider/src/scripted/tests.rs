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
        premium_requests: None,
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
            // The offending events step follows a failing stream and its
            // bound fail step, so its index is not 0 and counts both.
            vec![
                ScriptStep::Events(vec![text("cut")]),
                ScriptStep::Fail(ProviderError::StreamCut),
                ScriptStep::Events(vec![done(), text("a"), usage_event(), stop()]),
            ],
            2,
            "a delta after tool calls done",
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
    for (steps, step, case) in cases {
        match Script::new(steps).unwrap_err() {
            ScriptError::Grammar { step: named, .. } => {
                assert_eq!(named, step, "{case}: names the offending step");
            }
            other => panic!("{case}: expected a grammar rejection, got {other:?}"),
        }
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
    // The usage record is complete, so only the status and family guard can
    // reject it; an incomplete usage object would be refused by the usage
    // decoder whatever the guard does.
    let usage_with_status =
        format!(r#"{{"kind":"usage","usage":{FULL_USAGE},"status":413,"family":"openai_chat"}}"#);
    for line in [
        r#"{"kind":"fail","message":"x","status":413}"#,
        r#"{"kind":"fail","message":"x","family":"openai_chat"}"#,
        usage_with_status.as_str(),
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

const FULL_USAGE: &str = concat!(
    r#"{"input_tokens":3,"cached_input_tokens":1,"output_tokens":4,"#,
    r#""reasoning_tokens":2,"cache_write_tokens":5,"cost_usd":null}"#,
);

/// The events the single events step of `line` serves, which must end
/// cleanly.
fn replayed_events(line: &str) -> Vec<StreamEvent> {
    let script = Script::from_replay(line.as_bytes()).expect("the fixture decodes");
    let mut stream = script.open().expect("the events step opens");
    drain(&mut stream)
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("the stream ends cleanly")
}

#[test]
fn replay_decodes_every_event_kind_from_independent_wire_literals() {
    // The literal is hand-written wire text, not the encoder's output, so a
    // decoder and an encoder cannot drift together unnoticed. Argument bytes
    // that are not UTF-8 only fit `fragment_bytes`.
    let line = format!(
        concat!(
            r#"{{"kind":"events","events":["#,
            r#"{{"type":"reasoning_delta","text":"mull"}},"#,
            r#"{{"type":"text_delta","text":"Hi"}},"#,
            r#"{{"type":"tool_call_started","id":"c1","name":"read"}},"#,
            r#"{{"type":"tool_args_delta","id":"c1","fragment":"{{\"p\":"}},"#,
            r#"{{"type":"tool_args_delta","id":"c1","fragment_bytes":[255,0,34]}},"#,
            r#"{{"type":"replay","payload":{{"family":"openai_responses","model":"m","item":{{"type": "reasoning"}}}}}},"#,
            r#"{{"type":"tool_calls_done","calls":["#,
            r#"{{"id":"c1","name":"read","args":{{"kind":"parsed","value":{{"p": 1}}}}}},"#,
            r#"{{"id":"c2","name":"bad","args":{{"kind":"invalid","message":"eof"}}}},"#,
            r#"{{"id":"c3","name":"cut","args":{{"kind":"truncated"}}}}]}},"#,
            r#"{{"type":"usage","usage":{usage}}},"#,
            r#"{{"type":"stop","reason":"max_tokens"}}]}}"#,
        ),
        usage = FULL_USAGE,
    );
    let call = |id: &str, name: &str, args: ToolArgs| ToolCall {
        id: id.into(),
        name: name.into(),
        args,
    };
    assert_eq!(
        replayed_events(&line),
        vec![
            StreamEvent::ReasoningDelta {
                text: "mull".into()
            },
            text("Hi"),
            StreamEvent::ToolCallStarted {
                id: "c1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolArgsDelta {
                id: "c1".into(),
                fragment: br#"{"p":"#.to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "c1".into(),
                fragment: vec![255, 0, 34],
            },
            StreamEvent::Replay {
                payload: ReplayPayload {
                    family: Family::Responses,
                    model: "m".into(),
                    item: RawJson::parse(r#"{"type": "reasoning"}"#).unwrap(),
                },
            },
            StreamEvent::ToolCallsDone {
                calls: vec![
                    call(
                        "c1",
                        "read",
                        ToolArgs::Parsed(RawJson::parse(r#"{"p": 1}"#).unwrap()),
                    ),
                    call(
                        "c2",
                        "bad",
                        ToolArgs::Invalid {
                            message: "eof".into()
                        },
                    ),
                    call("c3", "cut", ToolArgs::Truncated),
                ],
            },
            StreamEvent::Usage {
                usage: Usage {
                    input_tokens: 3,
                    cached_input_tokens: 1,
                    output_tokens: 4,
                    reasoning_tokens: Some(2),
                    cache_write_tokens: 5,
                    premium_requests: None,
                    cost_usd: None,
                },
            },
            StreamEvent::Stop {
                reason: StopReason::MaxTokens,
            },
        ]
    );
}

#[test]
fn replay_maps_each_stop_reason_and_keeps_unknown_ones_verbatim() {
    let stop_of = |reason: &str| {
        let line = format!(
            r#"{{"kind":"events","events":[{{"type":"tool_calls_done","calls":[]}},{{"type":"usage","usage":{FULL_USAGE}}},{{"type":"stop","reason":{}}}]}}"#,
            sonic_rs::to_string(reason).expect("escapes"),
        );
        match replayed_events(&line).pop() {
            Some(StreamEvent::Stop { reason }) => reason,
            other => panic!("the last event is not a stop: {other:?}"),
        }
    };
    for (wire, expected) in [
        ("end_turn", StopReason::EndTurn),
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("refusal", StopReason::Refusal),
        ("paused", StopReason::Paused),
        // Anything else rides through byte for byte: no case folding, no
        // trimming, no empty-to-default.
        ("pause_turn", StopReason::Other("pause_turn".into())),
        ("END_TURN", StopReason::Other("END_TURN".into())),
        (" end_turn", StopReason::Other(" end_turn".into())),
        ("", StopReason::Other(String::new())),
    ] {
        assert_eq!(stop_of(wire), expected, "{wire:?}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table of refusal cases, each a one-line fixture"
)]
fn replay_rejects_contradictory_or_malformed_records_naming_the_physical_line() {
    let unknown = "unknown or malformed step";
    let usage_member = format!(r#","usage":{FULL_USAGE}"#);
    let events = |event: &str| format!(r#"{{"kind":"events","events":[{event}]}}"#);
    let args = |args: &str| {
        events(&format!(
            r#"{{"type":"tool_calls_done","calls":[{{"id":"c","name":"n","args":{args}}}]}}"#
        ))
    };
    let cases: Vec<(&str, String, usize, &str)> = vec![
        // Blank and whitespace-only lines are skipped but still counted.
        (
            "blank and CRLF lines before the bad record",
            String::from("\n\r\n  \n{\"kind\":\"usage\"}\n"),
            4,
            "usage is required",
        ),
        (
            "blank lines between a good record and a bad one",
            String::from("{\"kind\":\"fail\",\"message\":\"x\"}\n\n\n{\"kind\":\"bogus\"}"),
            4,
            unknown,
        ),
        // A step kind with a member that belongs to another kind is refused.
        (
            "events with a message",
            String::from(r#"{"kind":"events","events":[],"message":"x"}"#),
            1,
            unknown,
        ),
        (
            "fail with events",
            String::from(r#"{"kind":"fail","message":"x","events":[]}"#),
            1,
            unknown,
        ),
        (
            "fail with usage",
            format!(r#"{{"kind":"fail","message":"x"{usage_member}}}"#),
            1,
            unknown,
        ),
        (
            "usage with a message",
            format!(r#"{{"kind":"usage"{usage_member},"message":"x"}}"#),
            1,
            unknown,
        ),
        (
            "compact with a status",
            String::from(r#"{"kind":"compact","outcome":{"kind":"unsupported"},"status":500}"#),
            1,
            unknown,
        ),
        // A required member that is missing.
        (
            "events without events",
            String::from(r#"{"kind":"events"}"#),
            1,
            "events is required",
        ),
        (
            "fail without a message",
            String::from(r#"{"kind":"fail"}"#),
            1,
            "message is required",
        ),
        (
            "compact without an outcome",
            String::from(r#"{"kind":"compact"}"#),
            1,
            "outcome is required",
        ),
        (
            "unsupported outcome carrying items",
            String::from(r#"{"kind":"compact","outcome":{"kind":"unsupported","items":[]}}"#),
            1,
            "invalid compact outcome",
        ),
        (
            "compacted outcome without a model",
            String::from(
                r#"{"kind":"compact","outcome":{"kind":"compacted","family":"openai_codex","items":[]}}"#,
            ),
            1,
            "invalid compact outcome",
        ),
        // Events.
        (
            "unknown event type",
            events(r#"{"type":"mystery"}"#),
            1,
            "unknown event type",
        ),
        (
            "event with an unknown member",
            events(r#"{"type":"text_delta","text":"a","extra":1}"#),
            1,
            "invalid text delta",
        ),
        (
            "both fragment spellings",
            events(r#"{"type":"tool_args_delta","id":"c","fragment":"a","fragment_bytes":[97]}"#),
            1,
            "invalid tool argument delta",
        ),
        (
            "neither fragment spelling",
            events(r#"{"type":"tool_args_delta","id":"c"}"#),
            1,
            "invalid tool argument delta",
        ),
        (
            "a fragment byte over 255",
            events(r#"{"type":"tool_args_delta","id":"c","fragment_bytes":[256]}"#),
            1,
            "invalid tool argument delta",
        ),
        // Tool arguments: each kind carries exactly its own member.
        (
            "truncated arguments with a value",
            args(r#"{"kind":"truncated","value":{}}"#),
            1,
            "invalid tool arguments",
        ),
        (
            "parsed arguments with a message",
            args(r#"{"kind":"parsed","value":{},"message":"m"}"#),
            1,
            "invalid tool arguments",
        ),
        (
            "parsed arguments without a value",
            args(r#"{"kind":"parsed"}"#),
            1,
            "invalid tool arguments",
        ),
        (
            "invalid arguments without a message",
            args(r#"{"kind":"invalid"}"#),
            1,
            "invalid tool arguments",
        ),
        (
            "an unknown arguments kind",
            args(r#"{"kind":"weird"}"#),
            1,
            "invalid tool arguments",
        ),
    ];
    for (label, fixture, line, detail) in cases {
        assert_eq!(
            Script::from_replay(fixture.as_bytes()).unwrap_err(),
            ScriptError::ReplayFormat { line, detail },
            "{label}"
        );
    }
    assert_eq!(
        Script::from_replay(b"{\"kind\":\"fail\",\"message\":\"\xff\"}").unwrap_err(),
        ScriptError::ReplayFormat {
            line: 1,
            detail: "fixture is not UTF-8",
        }
    );
}

#[test]
fn a_cut_is_legal_at_every_grammar_position_only_with_its_bound_fail_step() {
    let turn = [text("a"), done(), usage_event(), stop()];
    // Cut before every event of the turn: nothing, after text, after tool
    // calls done, after usage. Each is a valid beginning only when a fail
    // step directly follows.
    for kept in 0..turn.len() {
        let prefix = turn[..kept].to_vec();
        assert_eq!(
            Script::new(vec![ScriptStep::Events(prefix.clone())]).unwrap_err(),
            ScriptError::Grammar {
                step: 0,
                detail: "events without stop must be followed by a fail step",
            },
            "{kept} events and no fail step"
        );
        let script = Script::new(vec![
            ScriptStep::Events(prefix.clone()),
            ScriptStep::Fail(ProviderError::StreamCut),
        ])
        .unwrap();
        assert_eq!(script.remaining(), 1, "{kept}: the pair is one operation");
        let mut stream = script.open().unwrap();
        let seen = drain(&mut stream);
        assert_eq!(seen.len(), kept + 1, "{kept}");
        assert_eq!(
            seen[..kept]
                .iter()
                .map(|item| item.as_ref().unwrap().clone())
                .collect::<Vec<_>>(),
            prefix,
            "{kept}"
        );
        assert!(
            matches!(seen[kept], Err(ProviderError::StreamCut)),
            "{kept}"
        );
        assert_eq!(script.aborted_streams(), 1, "{kept}");
    }
    // A turn that already stopped needs no fail step, and a fail step after
    // it is a separate operation, not the stream's terminal.
    let script = Script::new(vec![
        ScriptStep::Events(turn.to_vec()),
        ScriptStep::Fail(ProviderError::Overloaded),
    ])
    .unwrap();
    assert_eq!(script.remaining(), 2);
    let mut stream = script.open().unwrap();
    assert!(drain(&mut stream).iter().all(Result::is_ok));
    assert!(matches!(script.open(), Err(ProviderError::Overloaded)));
    assert_eq!(script.aborted_streams(), 0);
}

proptest::proptest! {
    /// The replay decoder must never panic on arbitrary input: every failure
    /// path surfaces a typed `ScriptError`, never a crash or silent misdecode.
    #[test]
    fn from_replay_never_panics_on_arbitrary_bytes(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..=512),
    ) {
        // `from_replay` documents exactly two failures: `ReplayFormat` for
        // invalid UTF-8 or an untyped line, `Grammar` for a terminal-grammar
        // violation. Any other outcome is an untyped hole.
        if let Err(error) = Script::from_replay(&bytes) {
            assert!(
                matches!(
                    error,
                    ScriptError::ReplayFormat { .. } | ScriptError::Grammar { .. }
                ),
                "untyped replay failure: {error:?}"
            );
        }
    }

    /// Near-miss lines: a well-formed `events` envelope whose event objects are
    /// fuzzed must decode or fail typed — never panic.
    #[test]
    fn from_replay_fuzzed_event_objects(
        payloads in proptest::collection::vec("[ -~]{0,60}", 1..=4),
    ) {
        let events: Vec<String> = payloads.iter().map(|p| format!("{{{p}}}")).collect();
        let line = format!("{{\"kind\":\"events\",\"events\":[{}]}}", events.join(","));
        if let Err(error) = Script::from_replay(line.as_bytes()) {
            assert!(
                matches!(
                    error,
                    ScriptError::ReplayFormat { .. } | ScriptError::Grammar { .. }
                ),
                "untyped replay failure: {error:?}"
            );
        }
    }

    /// A grammar-valid turn whose scalar fields are fuzzed must decode, and
    /// every field must reach the served stream byte for byte: ids, names,
    /// token counts, and any stop reason the decoder has no name for. (A
    /// turn that breaks the grammar, or a usage object missing a counter,
    /// would only ever exercise the rejection path.)
    #[test]
    fn from_replay_typed_events_with_fuzzed_fields(
        reason in "[ -~]{0,40}",
        call_id in "[ -~]{0,24}",
        tool_name in "[ -~]{0,24}",
        tokens in proptest::prelude::any::<u64>(),
    ) {
        proptest::prop_assume!(!matches!(
            reason.as_str(),
            "end_turn" | "tool_use" | "max_tokens" | "refusal" | "paused"
        ));
        let events = format!(
            "[{{\"type\":\"tool_call_started\",\"id\":{i},\"name\":{n}}},\
              {{\"type\":\"tool_calls_done\",\"calls\":[]}},\
              {{\"type\":\"usage\",\"usage\":{{\"input_tokens\":{t},\"cached_input_tokens\":0,\
              \"output_tokens\":{t},\"reasoning_tokens\":null,\"cache_write_tokens\":0,\
              \"cost_usd\":null}}}},\
              {{\"type\":\"stop\",\"reason\":{r}}}]",
            i = sonic_rs::to_string(&call_id).expect("escapes"),
            n = sonic_rs::to_string(&tool_name).expect("escapes"),
            t = tokens,
            r = sonic_rs::to_string(&reason).expect("escapes"),
        );
        let line = format!("{{\"kind\":\"events\",\"events\":{events}}}");
        let script = Script::from_replay(line.as_bytes()).expect("a grammar-valid turn decodes");
        let mut stream = script.open().expect("the events step opens");
        let served = drain(&mut stream)
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("the stream ends cleanly");
        assert_eq!(
            served,
            vec![
                StreamEvent::ToolCallStarted {
                    id: call_id,
                    name: tool_name,
                },
                done(),
                StreamEvent::Usage {
                    usage: Usage {
                        input_tokens: tokens,
                        cached_input_tokens: 0,
                        output_tokens: tokens,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        premium_requests: None,
                        cost_usd: None,
                    },
                },
                StreamEvent::Stop {
                    reason: StopReason::Other(reason),
                },
            ]
        );
    }
}
