//! A restart that lands on a dangling tool call synthesizes one error result
//! per call and aborts the turn: a dangling `ask` carries the exact restart
//! text, every other tool keeps the generic crash text.

#![expect(clippy::expect_used, reason = "replay fixtures fail loudly")]
#![expect(clippy::panic, reason = "replay fixtures fail loudly")]

use std::num::NonZeroU64;

use dal_core::{
    AssistantStop, Block, CallId, Effect, Entry, EntryId, EntryKind, Family, Header, JournalPart,
    Phase, Product, RawJson, Record, SessionId, Timestamp, TurnEndStop, TurnId, Usage, Workspace,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RESTART_TEXT: &str = "The question did not survive the restart. Continue on best judgment.";
const CRASH_TEXT: &str = "Tool call was not completed: dalgon stopped before it finished.";

fn turn(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero turn"))
}

fn entry(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero entry"))
}

fn usage() -> Usage {
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

fn dangling_journal(workspace: Workspace) -> Vec<Record> {
    let session = SessionId::parse("01890f47-36b0-7cc4-8000-000000000001").expect("session id");
    let at = Timestamp::now();
    let ask = CallId::new("ask-call");
    let read = CallId::new("read-call");
    let assistant = Entry {
        id: entry(1),
        parent: None,
        at,
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test".into(),
            provider: None,
            route: None,
            content: vec![
                Block::ToolCall {
                    id: ask.clone(),
                    name: "ask".into(),
                    input: RawJson::parse("{}").expect("args"),
                    replay: None,
                },
                Block::ToolCall {
                    id: read.clone(),
                    name: "read".into(),
                    input: RawJson::parse("{}").expect("args"),
                    replay: None,
                },
            ],
            usage: usage(),
            stop: AssistantStop::ToolUse,
            replay: None,
        },
    };
    vec![
        Record::Session(Header {
            id: session,
            at,
            workspace,
            product: Product::Dal,
            from: None,
        }),
        Record::TurnReserved { at, turn: turn(1) },
        Record::TurnReserved { at, turn: turn(2) },
        Record::TurnReserved { at, turn: turn(3) },
        Record::TurnStart { at, turn: turn(3) },
        Record::Assistant(assistant),
        Record::ToolStart {
            at,
            turn: turn(3),
            call: ask,
        },
        Record::ToolStart {
            at,
            turn: turn(3),
            call: read,
        },
    ]
}

/// One assistant entry with four calls: `ask`, `task` and `Ask` never settle
/// (`Ask` never even started), while `read` already journaled its result.
fn mixed_journal(workspace: Workspace) -> Vec<Record> {
    let session = SessionId::parse("01890f47-36b0-7cc4-8000-000000000002").expect("session id");
    let at = Timestamp::now();
    let call = |id: &str, name: &str| Block::ToolCall {
        id: CallId::new(id),
        name: name.into(),
        input: RawJson::parse("{}").expect("args"),
        replay: None,
    };
    let assistant = Entry {
        id: entry(1),
        parent: None,
        at,
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test".into(),
            provider: None,
            route: None,
            content: vec![
                call("ask-call", "ask"),
                call("task-call", "task"),
                call("shout-call", "Ask"),
                call("read-call", "read"),
            ],
            usage: usage(),
            stop: AssistantStop::ToolUse,
            replay: None,
        },
    };
    let settled = Entry {
        id: entry(2),
        parent: Some(entry(1)),
        at,
        kind: EntryKind::ToolResult {
            call: CallId::new("read-call"),
            name: "read".into(),
            error: false,
            parts: vec![JournalPart::Text {
                text: "file text".into(),
            }],
            changes: Vec::new(),
            elapsed_ms: None,
        },
    };
    vec![
        Record::Session(Header {
            id: session,
            at,
            workspace,
            product: Product::Dal,
            from: None,
        }),
        Record::TurnReserved { at, turn: turn(1) },
        Record::TurnStart { at, turn: turn(1) },
        Record::Assistant(assistant),
        Record::ToolStart {
            at,
            turn: turn(1),
            call: CallId::new("ask-call"),
        },
        Record::ToolStart {
            at,
            turn: turn(1),
            call: CallId::new("task-call"),
        },
        Record::ToolStart {
            at,
            turn: turn(1),
            call: CallId::new("read-call"),
        },
        Record::ToolResult(settled),
    ]
}

fn synthesized_text(entry: &Entry) -> (&str, bool, &str) {
    let EntryKind::ToolResult {
        name, error, parts, ..
    } = &entry.kind
    else {
        panic!("repair appends tool results");
    };
    let [JournalPart::Text { text }] = parts.as_slice() else {
        panic!("one text part per synthesized result");
    };
    (name.as_ref(), *error, text.as_ref())
}

/// Repair touches only calls with no journaled result, and only the exact
/// tool name `ask` earns the question text: `task` and `Ask` share the
/// generic crash text, and the settled `read` is never answered twice.
#[test]
fn a_restart_repairs_only_open_calls_and_only_the_exact_ask_name_gets_the_ask_text() -> TestResult {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::new(tmp.path().to_path_buf()).expect("workspace");
    let (session, effects) = dal_core::Session::replay(mixed_journal(workspace), Timestamp::now())?;
    assert!(matches!(session.phase(), Phase::Idle));
    let [Effect::Emit(emit)] = effects.as_slice() else {
        panic!("replay must append one repair emit");
    };
    let mut repaired = Vec::new();
    for record in &emit.records {
        if let Record::ToolResult(entry) = record {
            let (name, error, text) = synthesized_text(entry);
            repaired.push((
                name.to_owned(),
                error,
                text.to_owned(),
                entry.id,
                entry.parent,
            ));
        }
    }
    let expected = |name: &str, text: &str, id: u64, parent: u64| {
        (
            name.to_owned(),
            true,
            text.to_owned(),
            entry(id),
            Some(entry(parent)),
        )
    };
    assert_eq!(
        repaired,
        vec![
            expected("ask", RESTART_TEXT, 3, 2),
            expected("task", CRASH_TEXT, 4, 3),
            expected("Ask", CRASH_TEXT, 5, 4),
        ],
        "only the three open calls are repaired, in call order, chained after the settled result"
    );
    let aborts = emit
        .records
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::TurnEnd {
                    stop: TurnEndStop::Aborted,
                    ..
                }
            )
        })
        .count();
    assert_eq!(aborts, 1, "the turn aborts exactly once after repair");
    Ok(())
}

#[test]
fn a_restart_synthesizes_the_ask_text_for_a_dangling_ask() -> TestResult {
    let tmp = tempfile::tempdir().expect("tempdir");
    let workspace = Workspace::new(tmp.path().to_path_buf()).expect("workspace");
    let records = dangling_journal(workspace);
    let (session, effects) = dal_core::Session::replay(records, Timestamp::now())?;
    assert!(matches!(session.phase(), Phase::Idle));
    let [Effect::Emit(emit)] = effects.as_slice() else {
        panic!("replay must append one repair emit");
    };
    let mut seen_ask = false;
    let mut seen_read = false;
    for record in &emit.records {
        let Record::ToolResult(entry) = record else {
            continue;
        };
        let (name, error, text) = synthesized_text(entry);
        assert!(error, "a synthesized result is an error result");
        match name {
            "ask" => {
                assert_eq!(text, RESTART_TEXT);
                seen_ask = true;
            }
            "read" => {
                assert_eq!(text, CRASH_TEXT);
                seen_read = true;
            }
            other => panic!("unexpected dangling call {other}"),
        }
    }
    assert!(seen_ask && seen_read, "both dangling calls are repaired");
    assert!(
        emit.records.iter().any(|record| matches!(
            record,
            Record::TurnEnd {
                stop: TurnEndStop::Aborted,
                ..
            }
        )),
        "the turn aborts after repair"
    );
    Ok(())
}
