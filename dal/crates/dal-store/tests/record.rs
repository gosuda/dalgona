#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
//! Format-1 codec round-trips, oracles, and refusal reasons.

mod support;

use std::num::NonZeroU64;

use dal_core::{
    Answer, ApprovalMode, AssistantStop, Block, CallId, ClientId, Entry, EntryId, EntryKind,
    Family, Gen, Header, InferredPurpose, JobEvent, JobId, JournalPart, Mail, MailMode, Mode,
    ModelRoute, Owner, Product, RawJson, Record, RequestId, SessionId, Source, ThinkingLevel,
    TurnEndStop, TurnId, Usage, Workspace, decode, encode, scan_head,
};
use dal_store::Store;
use proptest::prelude::*;
use support::temp_dir::TempDir;

use sonic_rs::JsonValueTrait;

fn timestamp() -> jiff::Timestamp {
    "2026-09-28T10:15:30.123Z"
        .parse()
        .expect("fixed test timestamp parses")
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
}

fn job_id() -> JobId {
    JobId::parse("0192aa00-0000-7000-8000-000000000001").expect("fixed job id parses")
}

fn request_id() -> RequestId {
    RequestId::parse("0192aa00-0000-7000-8000-000000000002").expect("fixed request id parses")
}

fn session_id() -> SessionId {
    SessionId::parse("0192aa00-0000-7000-8000-000000000003").expect("fixed session id parses")
}

fn workspace() -> Workspace {
    Workspace::new("/tmp/dal-store-record-corpus".into()).expect("absolute test workspace")
}

fn usage() -> Usage {
    Usage {
        input_tokens: 7,
        cached_input_tokens: 2,
        output_tokens: 5,
        reasoning_tokens: Some(1),
        cache_write_tokens: 3,
        cost_usd: None,
    }
}

fn user_entry(id: u64, text: &str) -> Entry {
    Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }
}
fn assistant_entry(id: u64) -> Entry {
    Entry {
        id: entry_id(id),
        parent: NonZeroU64::new(id.saturating_sub(1)).map(EntryId::new),
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            content: vec![Block::Text { text: "hi".into() }],
            usage: usage(),
            stop: AssistantStop::Done,
        },
    }
}

fn corpus() -> Vec<Record> {
    let at = timestamp();
    vec![
        Record::Session(Header {
            id: session_id(),
            at,
            workspace: workspace(),
            product: Product::Dalgona,
            from: Some(Source {
                session: session_id(),
                entry: Some(entry_id(1)),
            }),
        }),
        Record::Boot {
            at,
            r#gen: Gen::new(NonZeroU64::new(3).expect("nonzero gen")),
            version: "0.1.0".into(),
        },
        Record::User(user_entry(1, "hello")),
        Record::Assistant(assistant_entry(2)),
        Record::ToolResult(Entry {
            id: entry_id(3),
            parent: Some(entry_id(2)),
            at,
            kind: EntryKind::ToolResult {
                call: CallId::new("call-1"),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text { text: "out".into() }],
                changes: vec![],
            },
        }),
        Record::Reminder(Entry {
            id: entry_id(4),
            parent: Some(entry_id(3)),
            at,
            kind: EntryKind::Reminder {
                source: "rule:x".into(),
                text: "remember".into(),
            },
        }),
        Record::Model(Entry {
            id: entry_id(5),
            parent: Some(entry_id(4)),
            at,
            kind: EntryKind::Model {
                route: ModelRoute::Api {
                    family: Family::Chat,
                    model: "m".into(),
                },
            },
        }),
        Record::Thinking(Entry {
            id: entry_id(6),
            parent: Some(entry_id(5)),
            at,
            kind: EntryKind::Thinking {
                level: ThinkingLevel::Low,
            },
        }),
        Record::Approval(Entry {
            id: entry_id(7),
            parent: Some(entry_id(6)),
            at,
            kind: EntryKind::Approval {
                mode: ApprovalMode::Ask,
            },
        }),
        Record::Mode(Entry {
            id: entry_id(8),
            parent: Some(entry_id(7)),
            at,
            kind: EntryKind::Mode { mode: Mode::Normal },
        }),
        Record::Compaction(Entry {
            id: entry_id(9),
            parent: Some(entry_id(8)),
            at,
            kind: EntryKind::Compaction {
                summary: Some("cut".into()),
                first_kept: Some(entry_id(4)),
                tokens_before: 100,
                replay: None,
                usage: Some(usage()),
                parts: Vec::new(),
                parts_tokens: 0,
            },
        }),
        Record::BranchSummary(Entry {
            id: entry_id(10),
            parent: Some(entry_id(9)),
            at,
            kind: EntryKind::BranchSummary {
                from: entry_id(2),
                summary: "abandoned".into(),
            },
        }),
        Record::Leaf {
            at,
            to: Some(entry_id(10)),
        },
        Record::Label {
            at,
            entry: entry_id(1),
            label: Some("todo".into()),
        },
        Record::Name {
            at,
            name: Some("parser fix".into()),
        },
        Record::Archive { at, archived: true },
        Record::TurnStart {
            at,
            turn: turn_id(3),
        },
        Record::ToolStart {
            at,
            turn: turn_id(3),
            call: CallId::new("call-2"),
        },
        Record::TurnEnd {
            at,
            turn: turn_id(3),
            stop: TurnEndStop::Done,
            usage: Some(usage()),
            changes: vec![],
        },
        Record::RuleFired {
            at,
            turn: turn_id(3),
            rule: "r".into(),
            entry: entry_id(4),
        },
        Record::Resolved {
            at,
            request: request_id(),
            answer: Answer::Approve,
            by: ClientId::new("tester"),
            was_default: false,
        },
        Record::AllowAlways {
            at,
            tool: "read".into(),
            by: ClientId::new("tester"),
        },
        Record::GrantGiven {
            at,
            ext: "x".into(),
            set: vec!["read".into()],
            scope: "session".into(),
            by: ClientId::new("tester"),
        },
        Record::ScopedGrant {
            at,
            call: CallId::new("call-3"),
            prefix: vec!["git".into()],
            roots: vec!["/tmp".into()],
            job: job_id(),
            by: ClientId::new("tester"),
        },
        Record::ScopedGrantEnded { at, job: job_id() },
        Record::BeforeRequestMut {
            at,
            turn: turn_id(3),
            ext: "x".into(),
            field: "model".into(),
            old: "a".into(),
            new: "b".into(),
        },
        Record::ToolPromoted {
            at,
            tool: "read".into(),
            turn: Some(turn_id(3)),
            leaf: Some(entry_id(10)),
        },
        Record::WakeAttempt {
            at,
            turn: turn_id(3),
            count: 2,
            jobs: vec![job_id()],
        },
        Record::Job {
            at,
            job: job_id(),
            event: JobEvent::Started { kind: None },
        },
        Record::Ext {
            at,
            ext: "x".into(),
            kind: "k".into(),
            body: RawJson::parse("{}").expect("empty object parses"),
        },
        Record::Mail(Mail {
            at,
            from: session_id(),
            to: session_id(),
            mode: MailMode::NextTurn,
            text: "ping".into(),
            reply_to: Some("cursor-1".into()),
        }),
        Record::Inferred {
            at,
            who: Owner::Core,
            purpose: InferredPurpose::Synthetic {
                id: "test/model".into(),
            },
            usage: usage(),
        },
    ]
}

fn assert_oracle(line: &[u8], record: &Record) {
    let text = std::str::from_utf8(line).expect("canonical lines are UTF-8");
    let value: sonic_rs::Value = sonic_rs::from_str(text).expect("line parses as JSON");
    assert_eq!(
        value.get("v").and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(1),
        "oracle sees format version 1"
    );
    assert_eq!(
        value.get("type").and_then(sonic_rs::JsonValueTrait::as_str),
        Some(record.tag()),
        "oracle agrees on the record tag"
    );
    let typed = decode(line).expect("typed decode succeeds");
    assert_eq!(&typed.record, record, "typed decode matches");
    let again = encode(&typed.record).expect("typed re-encode succeeds");
    assert_eq!(again, line, "re-encode is byte-identical");
}

#[test]
fn roundtrip_literal_lines() {
    let records = corpus();
    assert!(
        records.len() >= 31,
        "corpus covers every format-1 record type"
    );
    for record in &records {
        let line = encode(record).expect("record encodes");
        let decoded = decode(&line).expect("line decodes");
        assert_eq!(&decoded.record, record, "decode inverts encode");
        let again = encode(&decoded.record).expect("re-encode succeeds");
        assert_eq!(again, line, "every line round-trips byte-for-byte");
    }
    let mail = records
        .iter()
        .find(|record| matches!(record, Record::Mail(_)))
        .expect("corpus holds Mail");
    let inferred = records
        .iter()
        .find(|record| matches!(record, Record::Inferred { .. }))
        .expect("corpus holds Inferred");
    for record in [mail, inferred] {
        let line = encode(record).expect("record encodes");
        assert_eq!(
            encode(&decode(&line).expect("decodes").record).expect("re-encodes"),
            line
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn proptest_roundtrip_against_value_oracle(index in 0usize..32, salt in 0u64..1000) {
        let records = corpus();
        let record = &records[index % records.len()];
        let line = encode(record).expect("record encodes");
        assert_oracle(&line, record);
        let text = format!("salt-{salt}");
        let salted = Record::User(user_entry(101, &text));
        let salted_line = encode(&salted).expect("salted record encodes");
        assert_oracle(&salted_line, &salted);
    }
}

#[test]
fn version_refusal_precedes_type_check() {
    let error = decode(b"{\"v\":2,\"type\":\"nonsense\"}\n").expect_err("v2 refuses");
    assert!(
        matches!(
            error,
            dal_core::DecodeError::UnsupportedVersion { found: 2 }
        ),
        "version refusal precedes the type check"
    );
    let repeat = decode(b"{\"v\":2,\"type\":\"nonsense\"}\n").expect_err("refusal is stable");
    assert_eq!(error, repeat, "refusal performs no write and repeats");
}

#[test]
fn missing_version_and_unknown_type_reasons() {
    let missing = decode(b"{\"type\":\"user\"}\n").expect_err("missing version fails");
    assert_eq!(
        missing.to_string(),
        "record has no format version",
        "exact missing-version reason"
    );
    let unknown = decode(b"{\"v\":1,\"type\":\"nope\"}\n").expect_err("unknown type fails");
    assert_eq!(
        unknown.to_string(),
        "unknown record type \"nope\"",
        "exact unknown-type reason"
    );
}

#[test]
fn unknown_member_dropped_on_decode() {
    let record = Record::User(user_entry(1, "hello"));
    let line = encode(&record).expect("record encodes");
    let text = std::str::from_utf8(&line).expect("UTF-8 line");
    let widened = text.replacen("{\"v\":1,", "{\"v\":1,\"future\":1,", 1);
    assert_ne!(widened.as_bytes(), line.as_slice());
    let decoded = decode(widened.as_bytes()).expect("unknown member decodes");
    assert_eq!(&decoded.record, &record, "typed value is unchanged");
    let again = encode(&decoded.record).expect("re-encode succeeds");
    assert_eq!(again, line, "unknown member is absent after re-encode");
}

#[test]
fn scan_head_matches_full_decode() {
    for record in corpus() {
        let line = encode(&record).expect("record encodes");
        let full = decode(&line).expect("full decode succeeds");
        match scan_head(&line) {
            Some(head) => {
                let entry = full
                    .record
                    .entry()
                    .expect("scanned records are tree records");
                assert_eq!(head.id, entry.id, "scan id matches full decode");
                assert_eq!(head.parent, entry.parent, "scan parent matches full decode");
                assert_eq!(
                    head.kind.tag(),
                    full.record.tag(),
                    "scan tag matches full decode"
                );
            }
            None => assert!(
                full.record.entry().is_none(),
                "meta records return no scan result"
            ),
        }
    }
    let meta = Record::Name {
        at: timestamp(),
        name: Some("x".into()),
    };
    let line = encode(&meta).expect("meta encodes");
    assert!(
        scan_head(&line).is_none(),
        "a meta record returns no scan result"
    );
}

fn store_in(temp: &TempDir) -> (Store, Workspace) {
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(
        temp.path().join("data"),
        workspace.clone(),
        Product::Dalgona,
    );
    (store, workspace)
}

#[tokio::test]
async fn mail_and_synthetic_inferred_records_replay() {
    let temp = TempDir::new("record-replay");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let from = SessionId::new_v7();
    let mail = Record::Mail(Mail {
        at: timestamp(),
        from,
        to: id,
        mode: MailMode::Steer,
        text: "review the diff".into(),
        reply_to: Some("cursor-9".into()),
    });
    let inferred = Record::Inferred {
        at: timestamp(),
        who: Owner::Core,
        purpose: InferredPurpose::Synthetic {
            id: "review/model".into(),
        },
        usage: usage(),
    };
    journal
        .append(vec![Record::User(user_entry(1, "start"))])
        .await
        .expect("user appends");
    journal
        .append(vec![mail.clone(), inferred.clone()])
        .await
        .expect("mail and inferred append");
    let before: Vec<Vec<u8>> = journal
        .records()
        .iter()
        .map(|record| encode(record).expect("record encodes"))
        .collect();
    let boot_gen = |records: &[Record]| {
        records
            .iter()
            .rev()
            .find_map(|record| match record {
                Record::Boot {
                    r#gen: generation, ..
                } => Some(generation.get()),
                _ => None,
            })
            .expect("a boot record exists")
    };
    let first_boot = boot_gen(journal.records());
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after: Vec<Vec<u8>> = reopened
        .records()
        .iter()
        .map(|record| encode(record).expect("record encodes"))
        .collect();
    assert_eq!(
        after.len(),
        before.len() + 1,
        "reopen appends exactly one recovery boot"
    );
    assert_eq!(
        after[..before.len()],
        before,
        "durable prefix survives reopen byte-for-byte"
    );
    assert_eq!(
        boot_gen(reopened.records()),
        first_boot + 1,
        "recovery boot increments the generation"
    );
    let back = reopened.records();
    let kept_mail = back
        .iter()
        .find_map(|record| match record {
            Record::Mail(mail) => Some(mail),
            _ => None,
        })
        .expect("mail survives");
    assert_eq!(kept_mail.from, from, "mail retains its sender");
    assert_eq!(kept_mail.to, id, "mail retains its recipient");
    assert_eq!(kept_mail.mode, MailMode::Steer, "mail retains its mode");
    assert_eq!(
        kept_mail.text.as_ref(),
        "review the diff",
        "mail retains its text"
    );
    assert_eq!(
        kept_mail.reply_to.as_deref(),
        Some("cursor-9"),
        "mail retains its reply cursor"
    );
    let kept_inferred = back
        .iter()
        .find_map(|record| match record {
            Record::Inferred {
                who,
                purpose,
                usage: kept,
                ..
            } => Some((who, purpose, kept)),
            _ => None,
        })
        .expect("inferred survives");
    assert_eq!(kept_inferred.0, &Owner::Core, "inferred retains its owner");
    assert_eq!(
        kept_inferred.1,
        &InferredPurpose::Synthetic {
            id: "review/model".into()
        },
        "inferred retains its synthetic id"
    );
    assert_eq!(kept_inferred.2, &usage(), "inferred retains its usage");
}
