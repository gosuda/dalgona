#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests fail loudly"
)]

//! Shared-contract regression tests for the journal codec.
//!
//! Covers call identity, terminal outcomes, cancellation ordering,
//! ownership boundaries, and decode/encode parity through public APIs.

use std::num::NonZeroU64;

use dal_core::{
    AssistantStop, Block, CallId, ClientId, Entry, EntryId, EntryKind, Family, JobEvent,
    JobOutcome, Owner, RawJson, Record, ResolutionCause, TurnEndStop, TurnId, Usage, decode,
    encode,
};

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("nonzero id")
}

fn test_usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    }
}

fn at() -> jiff::Timestamp {
    "2026-09-25T10:15:35.410Z"
        .parse()
        .expect("valid fixture timestamp")
}

fn assistant_with_stop(stop: AssistantStop) -> Record {
    Record::Assistant(Entry {
        id: EntryId::new(nz(5)),
        parent: None,
        at: at(),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "m".into(),
            provider: None,
            content: Vec::new(),
            usage: test_usage(),
            stop,
            route: None,
            replay: None,
        },
    })
}

fn turn_end_with_stop(stop: TurnEndStop) -> Record {
    Record::TurnEnd {
        at: at(),
        turn: TurnId::new(nz(1)),
        stop,
        usage: None,
        changes: Vec::new(),
    }
}

#[test]
fn call_identity_is_preserved_verbatim_without_normalization() {
    for raw in [
        "toolu_01",
        "ToolU_01",
        "call_1",
        "call with spaces",
        "CALL-9/X",
    ] {
        let id = CallId::new(raw);
        assert_eq!(id.as_str(), raw);
        assert_eq!(id.to_string(), raw);
    }
    assert_ne!(CallId::new("toolu_01"), CallId::new("ToolU_01"));
}

#[test]
fn tool_call_id_survives_the_journal_codec() {
    let record = Record::Assistant(Entry {
        id: EntryId::new(nz(5)),
        parent: None,
        at: at(),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "m".into(),
            provider: None,
            content: vec![Block::ToolCall {
                id: CallId::new("toolu_01"),
                name: "read".into(),
                input: RawJson::parse("{}").expect("empty object"),
                replay: None,
            }],
            usage: test_usage(),
            stop: AssistantStop::ToolUse,
            route: None,
            replay: None,
        },
    });
    let encoded = encode(&record).expect("assistant encodes");
    let decoded = decode(&encoded).expect("assistant decodes").record;
    let Record::Assistant(entry) = decoded else {
        panic!("expected assistant record");
    };
    let EntryKind::Assistant { content, .. } = entry.kind else {
        panic!("expected assistant kind");
    };
    assert_eq!(content.len(), 1);
    let Block::ToolCall { id, .. } = &content[0] else {
        panic!("expected tool_call block");
    };
    assert_eq!(id.as_str(), "toolu_01");
}

#[test]
fn tool_result_call_survives_the_journal_codec() {
    let record = Record::ToolResult(Entry {
        id: EntryId::new(nz(6)),
        parent: Some(EntryId::new(nz(5))),
        at: at(),
        kind: EntryKind::ToolResult {
            call: CallId::new("toolu_01"),
            name: "read".into(),
            error: false,
            parts: Vec::new(),
            changes: Vec::new(),
            elapsed_ms: None,
        },
    });
    let encoded = encode(&record).expect("tool_result encodes");
    let decoded = decode(&encoded).expect("tool_result decodes").record;
    let Record::ToolResult(entry) = decoded else {
        panic!("expected tool_result record");
    };
    let EntryKind::ToolResult { call, .. } = entry.kind else {
        panic!("expected tool_result kind");
    };
    assert_eq!(call.as_str(), "toolu_01");
}

#[test]
fn tool_start_call_identity_round_trips_byte_for_byte() {
    let line = "{\"v\":1,\"type\":\"tool_start\",\"at\":\"2026-09-25T10:15:35.420Z\",\"turn\":1,\"call\":\"toolu_01\"}\n";
    let decoded = decode(line.as_bytes()).expect("tool_start decodes").record;
    let Record::ToolStart { call, .. } = decoded else {
        panic!("expected tool_start");
    };
    assert_eq!(call.as_str(), "toolu_01");
    let back = decode(line.as_bytes()).expect("decode").record;
    let re_encoded = encode(&back).expect("encode");
    assert_eq!(re_encoded.as_slice(), line.as_bytes());
}

#[test]
fn turn_end_stops_round_trip_with_distinct_literals() {
    let done = turn_end_with_stop(TurnEndStop::Done);
    let done_text = std::str::from_utf8(&encode(&done).expect("encodes"))
        .expect("utf8")
        .to_owned();
    assert!(done_text.contains("\"stop\":\"done\""));

    let cancelled = turn_end_with_stop(TurnEndStop::Cancelled);
    let cancelled_text = std::str::from_utf8(&encode(&cancelled).expect("encodes"))
        .expect("utf8")
        .to_owned();
    assert!(cancelled_text.contains("\"stop\":\"cancelled\""));

    let aborted = turn_end_with_stop(TurnEndStop::Aborted);
    let aborted_text = std::str::from_utf8(&encode(&aborted).expect("encodes"))
        .expect("utf8")
        .to_owned();
    assert!(aborted_text.contains("\"stop\":\"aborted\""));
    assert_ne!(cancelled_text, aborted_text);

    for stop in [
        TurnEndStop::Done,
        TurnEndStop::Length,
        TurnEndStop::Filter,
        TurnEndStop::MaxSteps,
        TurnEndStop::Cancelled,
        TurnEndStop::Aborted,
    ] {
        let record = turn_end_with_stop(stop.clone());
        let encoded = encode(&record).expect("turn_end encodes");
        let back = decode(&encoded).expect("turn_end decodes").record;
        let Record::TurnEnd {
            stop: back_stop, ..
        } = back
        else {
            panic!("expected turn_end");
        };
        assert_eq!(back_stop, stop);
    }

    let failed = turn_end_with_stop(TurnEndStop::Failed {
        message: "boom".into(),
    });
    let encoded = encode(&failed).expect("failed encodes");
    let back = decode(&encoded).expect("failed decodes").record;
    let Record::TurnEnd {
        stop: back_stop, ..
    } = back
    else {
        panic!("expected turn_end");
    };
    assert_eq!(back_stop, failed_stop());
}

fn failed_stop() -> TurnEndStop {
    TurnEndStop::Failed {
        message: "boom".into(),
    }
}

#[test]
fn assistant_stops_round_trip() {
    for stop in [
        AssistantStop::Done,
        AssistantStop::Length,
        AssistantStop::Filter,
        AssistantStop::ToolUse,
        AssistantStop::Cancelled,
    ] {
        let record = assistant_with_stop(stop.clone());
        let encoded = encode(&record).expect("assistant encodes");
        let back = decode(&encoded).expect("assistant decodes").record;
        let Record::Assistant(entry) = back else {
            panic!("expected assistant");
        };
        let EntryKind::Assistant {
            stop: back_stop, ..
        } = entry.kind
        else {
            panic!("expected assistant kind");
        };
        assert_eq!(back_stop, stop);
    }
    let failed = assistant_with_stop(AssistantStop::Failed {
        message: "nope".into(),
    });
    let encoded = encode(&failed).expect("failed encodes");
    let text = std::str::from_utf8(&encoded).expect("utf8").to_owned();
    assert!(text.contains("\"stop\""));
    let back = decode(&encoded).expect("failed decodes").record;
    let Record::Assistant(entry) = back else {
        panic!("expected assistant");
    };
    let EntryKind::Assistant {
        stop: back_stop, ..
    } = entry.kind
    else {
        panic!("expected assistant kind");
    };
    assert_eq!(
        back_stop,
        AssistantStop::Failed {
            message: "nope".into()
        }
    );
}

#[test]
fn job_outcomes_are_terminal_and_distinct() {
    check_job_outcome(&JobOutcome::Exited { code: 0 }, "\"exited\"");
    check_job_outcome(&JobOutcome::Cancelled, "\"cancelled\"");
    check_job_outcome(&JobOutcome::Lost, "\"lost\"");
    check_job_outcome(
        &JobOutcome::Failed {
            message: "x".into(),
        },
        "\"failed\"",
    );
}

fn check_job_outcome(outcome: &JobOutcome, fragment: &str) {
    let outcome = JobOutcome::clone(outcome);
    let record = Record::Job {
        at: at(),
        job: dal_core::JobId::new_v7(),
        event: JobEvent::Settled {
            outcome: Some(outcome.clone()),
        },
    };
    let encoded = encode(&record).expect("job encodes");
    let text = std::str::from_utf8(&encoded).expect("utf8").to_owned();
    assert!(text.contains(fragment));
    let back = decode(&encoded).expect("job decodes").record;
    let Record::Job { event, .. } = back else {
        panic!("expected job");
    };
    let JobEvent::Settled {
        outcome: Some(back_outcome),
    } = event
    else {
        panic!("expected settled outcome");
    };
    assert_eq!(back_outcome, outcome);
}

#[test]
fn cancelled_and_aborted_never_alias() {
    assert_ne!(TurnEndStop::Cancelled, TurnEndStop::Aborted);
    assert_ne!(AssistantStop::Cancelled, AssistantStop::Done);
    for line in [
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"cancelled\",\"usage\":null,\"changes\":[]}\n",
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"aborted\",\"usage\":null,\"changes\":[]}\n",
    ] {
        let decoded = decode(line.as_bytes()).expect("turn_end decodes").record;
        let re_encoded = encode(&decoded).expect("turn_end encodes");
        assert_eq!(re_encoded.as_slice(), line.as_bytes());
    }
    let cause: ResolutionCause = sonic_rs::from_str("\"cancelled\"").expect("cause");
    assert_eq!(cause, ResolutionCause::Cancelled);
}

#[test]
fn owner_core_and_extension_never_alias_across_the_codec() {
    assert_ne!(
        Owner::Core,
        Owner::Extension {
            name: "web".into(),
            origin: "bundled".into()
        }
    );
    check_owner_round_trip(&Owner::Core);
    check_owner_round_trip(&Owner::Extension {
        name: "web".into(),
        origin: "bundled".into(),
    });
    assert_ne!(ClientId::new("tui"), ClientId::new("print"));
    assert_eq!(ClientId::new("tui").as_str(), "tui");
}

fn check_owner_round_trip(who: &Owner) {
    let who = Owner::clone(who);
    let record = Record::Inferred {
        at: at(),
        who: who.clone(),
        purpose: dal_core::InferredPurpose::Synthetic { id: "m".into() },
        usage: test_usage(),
    };
    let encoded = encode(&record).expect("inferred encodes");
    let back = decode(&encoded).expect("inferred decodes").record;
    let Record::Inferred { who: back_who, .. } = back else {
        panic!("expected inferred");
    };
    assert_eq!(back_who, who);
}

#[test]
fn representative_records_encode_decode_encode_byte_identical() {
    let lines = [
        "{\"v\":1,\"type\":\"turn_start\",\"at\":\"2026-09-25T10:15:31.000Z\",\"turn\":1}\n",
        "{\"v\":1,\"type\":\"tool_start\",\"at\":\"2026-09-25T10:15:35.420Z\",\"turn\":1,\"call\":\"toolu_01\"}\n",
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":5400,\"output\":610,\"cache_read\":4200,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":18830},\"changes\":[{\"path\":\"lib/lexer.ml\",\"added\":12,\"removed\":3}]}\n",
        "{\"v\":1,\"type\":\"allow_always\",\"at\":\"2026-09-25T10:15:38.000Z\",\"tool\":\"exec\",\"by\":\"tui\"}\n",
    ];
    for line in lines {
        let decoded = decode(line.as_bytes()).expect("decodes").record;
        let once = encode(&decoded).expect("encodes");
        assert_eq!(once.as_slice(), line.as_bytes());
        let twice = encode(&decode(&once).expect("re-decodes").record).expect("re-encodes");
        assert_eq!(twice, once);
    }
}

#[test]
fn journal_rejects_future_versions_and_missing_versions() {
    let future = b"{\"v\":99,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}";
    assert!(matches!(
        decode(future),
        Err(dal_core::DecodeError::UnsupportedVersion { .. })
    ));
    assert!(matches!(
        decode(b"{}"),
        Err(dal_core::DecodeError::MissingVersion)
    ));
}

#[test]
fn inferred_owner_wire_spellings_are_pinned_and_foreign_spellings_are_refused() {
    let inferred = |who: Owner| Record::Inferred {
        at: at(),
        who,
        purpose: dal_core::InferredPurpose::Synthetic { id: "m".into() },
        usage: test_usage(),
    };
    let core = String::from_utf8(encode(&inferred(Owner::Core)).expect("core owner encodes"))
        .expect("journal line is utf-8");
    assert!(core.contains(r#""who":"core""#), "{core}");
    let web = String::from_utf8(
        encode(&inferred(Owner::Extension {
            name: "web".into(),
            origin: "bundled".into(),
        }))
        .expect("extension owner encodes"),
    )
    .expect("journal line is utf-8");
    assert!(
        web.contains(r#""who":{"extension":{"name":"web","origin":"bundled"}}"#),
        "{web}"
    );
    for foreign in [
        r#""extension""#,
        r#""Core""#,
        r#""""#,
        r#"{"core":null}"#,
        r#"{"extension":{"name":"web"}}"#,
        r#"{"extension":{"origin":"bundled"}}"#,
        r#"{"extension":"web"}"#,
        "null",
        "1",
    ] {
        let line = core.replace(r#""who":"core""#, &format!(r#""who":{foreign}"#));
        assert!(
            matches!(
                decode(line.as_bytes()),
                Err(dal_core::DecodeError::Invalid { .. })
            ),
            "accepted owner {foreign}"
        );
    }
}
