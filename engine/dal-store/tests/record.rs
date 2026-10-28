#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
//! Journal line codec round-trips, oracles, and refusal reasons.

mod support;

use std::{fs, num::NonZeroU64};

use dal_core::{
    Answer, ApprovalMode, AssistantRoute, AssistantStop, Block, CallId, ClientId, DeliveredMail,
    Entry, EntryId, EntryKind, EntryView, Family, Gen, Header, InferredPurpose, JobEvent, JobId,
    JournalPart, Mail, MailMode, Mode, ModelRoute, Owner, PrivateEffectId, PrivateLoopId,
    PrivateOutcome, Product, RawJson, Record, RequestId, SessionId, Source, ThinkingLevel,
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

fn private_loop_id() -> PrivateLoopId {
    PrivateLoopId::parse("0192aa00-0000-7000-8000-000000000004")
        .expect("fixed private loop id parses")
}

fn private_effect_id() -> PrivateEffectId {
    PrivateEffectId::parse("0192aa00-0000-7000-8000-000000000005")
        .expect("fixed private effect id parses")
}

fn workspace() -> Workspace {
    Workspace::new(std::env::temp_dir().join("dal-store-record-corpus"))
        .expect("absolute test workspace")
}

fn usage() -> Usage {
    Usage {
        input_tokens: 7,
        cached_input_tokens: 2,
        output_tokens: 5,
        reasoning_tokens: Some(1),
        cache_write_tokens: 3,
        premium_requests: None,
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
            provider: None,
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::Text {
                text: "hi".into(),
                replay: None,
            }],
            usage: usage(),
            stop: AssistantStop::Done,
            replay: None,
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
                elapsed_ms: Some(1234),
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
                    provider: None,
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
        Record::TurnReserved {
            at,
            turn: turn_id(3),
        },
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
            cause: None,
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
        Record::MailDelivered {
            at,
            turn: turn_id(3),
            entry: entry_id(11),
            mails: vec![
                DeliveredMail {
                    position: entry_id(1),
                    text: "ping".into(),
                },
                DeliveredMail {
                    position: entry_id(2),
                    text: "pong".into(),
                },
            ],
        },
        Record::Inferred {
            at,
            who: Owner::Core,
            purpose: InferredPurpose::Synthetic {
                id: "test/model".into(),
            },
            usage: usage(),
        },
        Record::PrivateLoop {
            at,
            r#loop: private_loop_id(),
            who: Owner::Extension {
                name: "history".into(),
                origin: "bundled".into(),
            },
            turn: Some(turn_id(3)),
        },
        Record::PrivateStart {
            at,
            r#loop: private_loop_id(),
            token: private_effect_id(),
            who: Owner::Core,
            turn: None,
            round: 2,
            call: CallId::new("call-private"),
            name: "read".into(),
            args: RawJson::parse(r#"{"path":"a.rs"}"#).expect("private args parse"),
        },
        Record::PrivateEnd {
            at,
            r#loop: private_loop_id(),
            token: private_effect_id(),
            outcome: PrivateOutcome::Settled {
                is_error: false,
                parts: vec![JournalPart::Text { text: "ok".into() }],
            },
        },
        Record::PrivateEnd {
            at,
            r#loop: private_loop_id(),
            token: private_effect_id(),
            outcome: PrivateOutcome::Indeterminate,
        },
    ]
}

fn assert_oracle(line: &[u8], record: &Record) {
    let text = std::str::from_utf8(line).expect("canonical lines are UTF-8");
    let value: sonic_rs::Value = sonic_rs::from_str(text).expect("line parses as JSON");
    // Turn reservations require format 2, mail deliveries format 3, and
    // every other record keeps format 1.
    let version = match record.tag() {
        "turn_reserved" => 2,
        "mail_delivered" => 3,
        _ => 1,
    };
    assert_eq!(
        value.get("v").and_then(sonic_rs::JsonValueTrait::as_u64),
        Some(version),
        "oracle sees the record's format version"
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
    fn proptest_roundtrip_against_value_oracle(index in 0usize..64, salt in 0u64..1000) {
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
    for version in [0, u64::from(dal_core::JOURNAL_VERSION) + 1] {
        let line = format!("{{\"v\":{version},\"type\":\"nonsense\"}}\n");
        let error = decode(line.as_bytes()).expect_err("unsupported version refuses");
        assert!(
            matches!(
                error,
                dal_core::DecodeError::UnsupportedVersion { found } if found == version
            ),
            "version refusal precedes the type check"
        );
    }
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

/// Appends one assistant replay line to a real durable store journal.
///
/// Returns the store handle plus the session id, the exact encoded line, and
/// the durable journal bytes. The store stays open for the caller: the live
/// journal owns the session, so the caller drives both lifecycles.
async fn durable_replay_journal(temp: &TempDir) -> (Store, SessionId, Vec<u8>, Vec<u8>) {
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![Record::User(user_entry(1, "start"))])
        .await
        .expect("user appends");
    let mut entry = assistant_entry(2);
    let EntryKind::Assistant { replay, .. } = &mut entry.kind else {
        unreachable!("assistant_entry holds an assistant kind");
    };
    *replay = Some(RawJson::parse(r#"{"reasoning":[1,2,3]}"#).expect("replay payload parses"));
    let record = Record::Assistant(entry);
    let line = encode(&record).expect("replay record encodes");
    journal
        .append(vec![record])
        .await
        .expect("assistant with replay appends");
    journal.close().await.expect("close the live journal");
    let on_disk = fs::read(store.session_file(id)).expect("read journal");
    (store, id, line, on_disk)
}

/// The expected view of the replayed assistant entry, built from the exact
/// encoded line so ids and parents come from the same source as the file.
fn expected_replay_view(line: &[u8]) -> EntryView {
    let decoded = decode(line).expect("replay line decodes");
    let Record::Assistant(mut entry) = decoded.record else {
        unreachable!("the staged line holds an assistant entry");
    };
    let EntryKind::Assistant { replay, .. } = &mut entry.kind else {
        unreachable!("the staged line holds an assistant kind");
    };
    *replay = Some(RawJson::parse(r#"{"reasoning":[1,2,3]}"#).expect("replay parses"));
    EntryView {
        id: entry.id,
        parent: entry.parent,
        kind: entry.kind,
    }
}

#[tokio::test]
async fn replayed_entry_reads_back_only_from_complete_lines() {
    let temp = TempDir::new("record-replay-boundary");
    let (store, id, line, on_disk) = durable_replay_journal(&temp).await;
    let journal_path = store.session_file(id);
    let root = temp.path().join("data");
    let replay = expected_replay_view(&line);
    let newline = on_disk.len() - line.len();
    for cut in newline..on_disk.len() {
        let truncated = &on_disk[..cut];
        fs::write(&journal_path, truncated).expect("stage a cut inside the replay line");
        let torn = Store::read_session_entry(&root, id, entry_id(2))
            .expect_err("the torn replay line is not an entry");
        assert!(
            matches!(&torn, dal_store::StoreError::UnknownEntry { id: found, entry }
                if *found == id && *entry == entry_id(2)),
            "a cut at {cut} reads as an unknown entry, got {torn:?}"
        );
        let user = Store::read_session_entry(&root, id, entry_id(1))
            .expect("the user entry stays reachable");
        assert_eq!(user.id, entry_id(1), "cut {cut}: the requested entry");
        assert_eq!(user.parent, None, "cut {cut}: the root has no parent");
        assert_eq!(
            fs::read(&journal_path).expect("read staged journal"),
            truncated,
            "a cut at {cut} never triggers repair or rewrite"
        );
    }
    fs::write(&journal_path, &on_disk).expect("restore the full journal");
    let found = Store::read_session_entry(&root, id, entry_id(2))
        .expect("the complete replay line is an entry");
    assert_eq!(found, replay, "the replay payload survives the read");
    for cut in 1..newline {
        let truncated = &on_disk[..cut];
        fs::write(&journal_path, truncated).expect("stage a cut inside earlier lines");
        assert!(
            Store::read_session_entry(&root, id, entry_id(2)).is_err(),
            "the replay entry is gone at cut {cut}"
        );
        assert_eq!(
            fs::read(&journal_path).expect("read staged journal"),
            truncated,
            "a cut at {cut} never triggers repair or rewrite"
        );
    }
    fs::write(&journal_path, &on_disk).expect("restore the full journal");
}

#[tokio::test]
async fn null_replay_append_is_refused_without_a_partial_write() {
    let temp = TempDir::new("record-null-replay");
    let (store, id, _, on_disk) = durable_replay_journal(&temp).await;
    let journal_path = store.session_file(id);
    let mut entry = assistant_entry(3);
    let EntryKind::Assistant { replay, .. } = &mut entry.kind else {
        unreachable!("assistant_entry holds an assistant kind");
    };
    *replay = Some(RawJson::parse("null").expect("explicit null parses"));
    let (mut reopened, _) = store
        .open_session(id)
        .await
        .expect("the durable session reopens");
    let baseline = fs::read(&journal_path).expect("read the reopened journal");
    assert!(
        baseline.len() > on_disk.len() && baseline.starts_with(&on_disk),
        "reopen appends its recovery boot after the durable prefix"
    );
    let error = reopened
        .append(vec![Record::Assistant(entry)])
        .await
        .expect_err("a null replay is refused at the append boundary");
    reopened.close().await.expect("close the reopened session");
    assert_eq!(
        error.to_string(),
        "cannot encode a null assistant replay",
        "exact NullReplay refusal"
    );
    assert_eq!(
        fs::read(&journal_path).expect("read the journal"),
        baseline,
        "the refused batch left the journal byte-identical"
    );
}

fn model_entry(id: u64, parent: u64, route: ModelRoute) -> Record {
    Record::Model(Entry {
        id: entry_id(id),
        parent: Some(entry_id(parent)),
        at: timestamp(),
        kind: EntryKind::Model { route },
    })
}

#[tokio::test]
async fn premium_usage_persists_across_reopen() {
    let temp = TempDir::new("record-premium");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let assistant_usage = Usage {
        input_tokens: 7,
        cached_input_tokens: 2,
        output_tokens: 5,
        reasoning_tokens: Some(1),
        cache_write_tokens: 3,
        premium_requests: Some(1.5),
        cost_usd: None,
    };
    let inferred_usage = Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        premium_requests: Some(2.25),
        cost_usd: None,
    };
    let assistant = Entry {
        id: entry_id(2),
        parent: Some(entry_id(1)),
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            provider: None,
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::Text {
                text: "hi".into(),
                replay: None,
            }],
            usage: assistant_usage,
            stop: AssistantStop::Done,
            replay: None,
        },
    };
    let inferred = Record::Inferred {
        at: timestamp(),
        who: Owner::Core,
        purpose: InferredPurpose::Synthetic {
            id: "review/model".into(),
        },
        usage: inferred_usage,
    };
    let turn_end = Record::TurnEnd {
        at: timestamp(),
        turn: turn_id(1),
        stop: TurnEndStop::Done,
        usage: Some(Usage {
            input_tokens: 8,
            cached_input_tokens: 2,
            output_tokens: 6,
            reasoning_tokens: None,
            cache_write_tokens: 3,
            premium_requests: Some(3.75),
            cost_usd: None,
        }),
        changes: vec![],
    };
    journal
        .append(vec![
            Record::User(user_entry(1, "start")),
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(1),
            },
            Record::Assistant(assistant),
            inferred,
            turn_end,
        ])
        .await
        .expect("premium turn appends");
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let back = reopened.records();
    let kept_assistant = back
        .iter()
        .find_map(|record| match record {
            Record::Assistant(entry) => match &entry.kind {
                EntryKind::Assistant { usage, .. } => Some(*usage),
                _ => None,
            },
            _ => None,
        })
        .expect("assistant survives");
    assert_eq!(
        kept_assistant.premium_requests,
        Some(1.5),
        "assistant premium survives the durable round-trip"
    );
    let kept_inferred = back
        .iter()
        .find_map(|record| match record {
            Record::Inferred { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("inferred survives");
    assert_eq!(
        kept_inferred.premium_requests,
        Some(2.25),
        "inferred premium survives the durable round-trip"
    );
    let kept_turn_end = back
        .iter()
        .find_map(|record| match record {
            Record::TurnEnd { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("turn end survives");
    assert_eq!(
        kept_turn_end
            .expect("turn end carries usage")
            .premium_requests,
        Some(3.75),
        "the summed turn premium survives the durable round-trip"
    );
}

fn reopened_records(journal: &[Record]) -> Vec<Vec<u8>> {
    journal
        .iter()
        .map(|record| encode(record).expect("record encodes"))
        .collect()
}

#[tokio::test]
async fn provider_model_record_persists_with_route_intact() {
    let temp = TempDir::new("record-provider-model");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![Record::User(user_entry(1, "start"))])
        .await
        .expect("user entry appends");
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "m".into(),
        provider: Some("acme".into()),
    };
    let model = model_entry(2, 1, route.clone());
    let text = std::str::from_utf8(&encode(&model).expect("model encodes"))
        .expect("journal line is utf-8")
        .to_owned();
    assert!(text.contains("\"provider\":\"acme\""), "{text}");
    journal
        .append(vec![model])
        .await
        .expect("provider model appends");
    let before = reopened_records(journal.records());
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after = reopened_records(reopened.records());
    assert_eq!(
        after.len(),
        before.len() + 1,
        "reopen appends exactly one recovery boot"
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "durable prefix survives reopen byte-for-byte"
    );
    let kept = reopened
        .records()
        .iter()
        .find_map(|record| match record {
            Record::Model(entry) => Some(entry),
            _ => None,
        })
        .expect("provider model record reopens");
    assert!(
        matches!(&kept.kind, EntryKind::Model { route: kept_route } if kept_route == &route),
        "the provider route did not survive reopen"
    );
}

#[tokio::test]
async fn legacy_provider_less_model_record_still_reads() {
    let temp = TempDir::new("record-legacy-model");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    journal
        .append(vec![Record::User(user_entry(1, "start"))])
        .await
        .expect("user entry appends");
    let model = model_entry(
        2,
        1,
        ModelRoute::Api {
            family: Family::Chat,
            model: "m".into(),
            provider: None,
        },
    );
    let text = std::str::from_utf8(&encode(&model).expect("model encodes"))
        .expect("journal line is utf-8")
        .to_owned();
    assert!(!text.contains("provider"), "{text}");
    journal
        .append(vec![model])
        .await
        .expect("legacy model appends");
    let before = reopened_records(journal.records());
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let after = reopened_records(reopened.records());
    assert_eq!(
        after.len(),
        before.len() + 1,
        "reopen appends exactly one recovery boot"
    );
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "durable prefix survives reopen byte-for-byte"
    );
    let kept = reopened
        .records()
        .iter()
        .find_map(|record| match record {
            Record::Model(entry) => Some(entry),
            _ => None,
        })
        .expect("legacy model record reopens");
    assert!(
        matches!(&kept.kind, EntryKind::Model { route } if route.provider().is_none()),
        "reopen invented a provider"
    );
}

#[tokio::test]
async fn sub_micro_premium_turn_agrees_across_reopen() {
    let temp = TempDir::new("record-premium-precision");
    let (store, _) = store_in(&temp);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let leg = 0.000_000_4;
    let assistant = Entry {
        id: entry_id(2),
        parent: Some(entry_id(1)),
        at: timestamp(),
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: "test-model".into(),
            provider: None,
            route: Some(AssistantRoute::Api(Family::Chat)),
            content: vec![Block::Text {
                text: "hi".into(),
                replay: None,
            }],
            usage: Usage {
                input_tokens: 7,
                cached_input_tokens: 2,
                output_tokens: 5,
                reasoning_tokens: Some(1),
                cache_write_tokens: 3,
                premium_requests: Some(leg),
                cost_usd: None,
            },
            stop: AssistantStop::Done,
            replay: None,
        },
    };
    let inferred = Record::Inferred {
        at: timestamp(),
        who: Owner::Core,
        purpose: InferredPurpose::Synthetic {
            id: "review/model".into(),
        },
        usage: Usage {
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: Some(leg),
            cost_usd: None,
        },
    };
    let turn_end = Record::TurnEnd {
        at: timestamp(),
        turn: turn_id(1),
        stop: TurnEndStop::Done,
        usage: Some(Usage {
            input_tokens: 8,
            cached_input_tokens: 2,
            output_tokens: 6,
            reasoning_tokens: None,
            cache_write_tokens: 3,
            premium_requests: Some(leg + leg),
            cost_usd: None,
        }),
        changes: vec![],
    };
    journal
        .append(vec![
            Record::User(user_entry(1, "start")),
            Record::TurnStart {
                at: timestamp(),
                turn: turn_id(1),
            },
            Record::Assistant(assistant),
            inferred,
            turn_end,
        ])
        .await
        .expect("sub-micro premium turn appends");
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    let back = reopened.records();
    let legs = [
        back.iter()
            .find_map(|record| match record {
                Record::Assistant(entry) => match &entry.kind {
                    EntryKind::Assistant { usage, .. } => Some(usage.premium_requests),
                    _ => None,
                },
                _ => None,
            })
            .expect("assistant survives"),
        back.iter()
            .find_map(|record| match record {
                Record::Inferred { usage, .. } => Some(usage.premium_requests),
                _ => None,
            })
            .expect("inferred survives"),
    ];
    let kept_total = back
        .iter()
        .find_map(|record| match record {
            Record::TurnEnd { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("turn end survives")
        .expect("turn end carries usage")
        .premium_requests;
    let mut known = true;
    let mut sum = 0.0;
    for premium in legs {
        match premium {
            Some(value) => {
                sum += value;
            }
            None => {
                known = false;
            }
        }
    }
    assert_eq!(
        kept_total,
        known.then_some(sum),
        "the reopened turn total agrees with its reopened legs"
    );
}
