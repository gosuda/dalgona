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
            route: None,
            content: vec![
                Block::ToolCall {
                    id: ask.clone(),
                    name: "ask".into(),
                    input: RawJson::parse("{}").expect("args"),
                },
                Block::ToolCall {
                    id: read.clone(),
                    name: "read".into(),
                    input: RawJson::parse("{}").expect("args"),
                },
            ],
            usage: usage(),
            stop: AssistantStop::ToolUse,
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
