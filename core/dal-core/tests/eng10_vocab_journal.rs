#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

//! ENG-10 vocabulary proofs: journal record codec (VOC-07A), pure
//! branch/fork projection (VOC-07B), and curated `lib.rs` exports (VOC-08A).
//!
//! These tests exercise only the public `dal_core::` API. The sibling
//! `journal/tests.rs` unit suite owns byte-canonical vectors; this file pins
//! the boundary and state-transition cases the unit suite leaves open:
//! version-shape rejection, `v`-before-`type` ordering, framing, member
//! order, closed `Mail` modes, route provenance, resolution causes, branch
//! error states, cycle termination, and the export surface.

use std::num::NonZeroU64;

use dal_core::{
    AssistantRoute, AssistantStop, BEFORE_TURN_SOURCE, BranchError, BranchMode, CallId,
    DecodeError, EncodeError, Entry, EntryId, EntryKind, Family, Header, InferredPurpose,
    JOURNAL_VERSION, JournalPart, MAX_RECORD, Mail, MailMode, ModelRoute, Owner, Product, RawJson,
    Record, RequestId, ResolutionCause, SessionId, Source, Timestamp, TurnId, Usage, Workspace,
    branch, decode, encode, scan_head,
};

fn nz(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test ids are nonzero")
}

fn eid(value: u64) -> EntryId {
    EntryId::new(nz(value))
}

fn tid(value: u64) -> TurnId {
    TurnId::new(nz(value))
}

fn stamp() -> Timestamp {
    "2026-09-25T10:15:30.000Z"
        .parse()
        .expect("fixed stamp parses")
}

fn header() -> Header {
    Header {
        id: SessionId::new_v7(),
        at: stamp(),
        workspace: Workspace::new("/tmp/eng10-vocab-journal".into()).expect("absolute workspace"),
        product: Product::Dal,
        from: None,
    }
}

fn user_entry(id: u64, parent: Option<u64>, text: &str) -> Entry {
    Entry {
        id: eid(id),
        parent: parent.map(eid),
        at: stamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }
}

fn user_record(id: u64, parent: Option<u64>, text: &str) -> Record {
    Record::User(user_entry(id, parent, text))
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

fn assistant_record(model: &str, route: Option<AssistantRoute>) -> Record {
    Record::Assistant(Entry {
        id: eid(5),
        parent: None,
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: model.into(),
            content: Vec::new(),
            usage: usage(),
            stop: AssistantStop::Done,
            route,
            replay: None,
        },
    })
}

fn assistant_line(model: &str, route_member: &str) -> String {
    format!(
        "{{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"{model}\",\"content\":[],\"usage\":{{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}},\"stop\":\"done\"{route_member}}}\n"
    )
}

fn leaf_line() -> &'static [u8] {
    b"{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}\n"
}

fn resolved_line(request: &RequestId, tail: &str) -> Vec<u8> {
    format!(
        "{{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:36.000Z\",\"request\":\"{request}\",\"answer\":\"cancel\",\"by\":\"core\"{tail}}}\n"
    )
    .into_bytes()
}

// ---- VOC-07A: version gate ----

#[test]
fn version_zero_is_unsupported_not_missing() {
    let line = b"{\"v\":0,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}";
    assert!(matches!(
        decode(line),
        Err(DecodeError::UnsupportedVersion { found: 0 })
    ));
}

#[test]
fn malformed_version_shapes_are_invalid_not_unsupported() {
    // Negative, string, float, overflowing, and null versions all fail the
    // unsigned-integer parse, so they are `Invalid`, not `UnsupportedVersion`.
    for line in [
        "{\"v\":-1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}",
        "{\"v\":\"1\",\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}",
        "{\"v\":1.0,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}",
        "{\"v\":18446744073709551616,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}",
        "{\"v\":null,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}",
    ] {
        match decode(line.as_bytes()) {
            Err(DecodeError::Invalid { message, .. }) => {
                assert!(
                    message.contains("unsigned integer"),
                    "unexpected message for {line}: {message}"
                );
            }
            other => panic!("expected Invalid for {line}, got {other:?}"),
        }
    }
    assert!(matches!(
        decode(b"{\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}"),
        Err(DecodeError::MissingVersion)
    ));
}

#[test]
fn version_is_checked_before_type_and_payload() {
    // Garbage payloads and reordered members still report the version first.
    // Every line below is well-formed JSON: version precedence applies to a
    // scanned envelope, never to malformed bytes. Format 2 is supported (it
    // carries `turn_reserved`), so only versions outside 1..=2 report
    // `UnsupportedVersion` here.
    for (line, expected) in [
        ("{\"v\":3,\"type\":42,\"garbage\":[]}", 3),
        ("{\"type\":\"leaf\",\"to\":1,\"v\":0}", 0),
        ("{\"to\":1,\"v\":3,\"type\":\"leaf\"}", 3),
        ("{\"v\":3,\"type\":\"leaf\",\"at\":[],\"to\":{}}", 3),
        (
            "{\"v\":9,\"type\":\"leaf\",\"type\":\"leaf\",\"at\":\"x\",\"to\":1}",
            9,
        ),
    ] {
        assert!(
            matches!(
                decode(line.as_bytes()),
                Err(DecodeError::UnsupportedVersion { found })
                if found == expected
            ),
            "{line}"
        );
    }
    // A malformed envelope never scans, so no version can be reported:
    // malformed JSON stays `Invalid` even when an unsupported `v` is visible.
    assert!(matches!(
        decode(b"{\"v\":3,\"type\":42,\"garbage\":[}"),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn legacy_kind_at_v2_and_reservation_at_v1_are_invalid() {
    // Only `turn_reserved` may ride format 2; only legacy kinds ride format 1.
    let legacy_at_v2 =
        b"{\"v\":2,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}";
    assert!(matches!(
        decode(legacy_at_v2),
        Err(DecodeError::Invalid { .. })
    ));
    let reserved_at_v1 =
        b"{\"v\":1,\"type\":\"turn_reserved\",\"at\":\"2026-10-13T00:00:00.000Z\",\"turn\":2}";
    assert!(matches!(
        decode(reserved_at_v1),
        Err(DecodeError::Invalid { .. })
    ));
    let reserved_at_v2 =
        b"{\"v\":2,\"type\":\"turn_reserved\",\"at\":\"2026-10-13T00:00:00.000Z\",\"turn\":2}";
    let decoded = decode(reserved_at_v2).expect("v2 reservation decodes");
    assert!(matches!(
        decoded.record,
        Record::TurnReserved { turn, .. } if turn.get() == 2
    ));
    let line = encode(&decoded.record).expect("v2 reservation encodes");
    assert!(line.starts_with(b"{\"v\":2,\"type\":\"turn_reserved\""));
}

#[test]
fn unknown_record_kind_at_v1_is_unknown_kind() {
    match decode(b"{\"v\":1,\"type\":\"teleport\",\"at\":\"2026-09-25T14:05:00.000Z\"}") {
        Err(DecodeError::UnknownRecordKind { kind }) => {
            assert_eq!(kind.as_ref(), "teleport");
        }
        other => panic!("expected UnknownRecordKind, got {other:?}"),
    }
}

// ---- VOC-07A: framing ----

#[test]
fn line_framing_rejects_empty_blank_non_utf8_and_trailing_bytes() {
    for line in [b"".as_slice(), b"   ", b"\t \r\n"] {
        assert!(
            matches!(decode(line), Err(DecodeError::Invalid { .. })),
            "blank line must be Invalid: {line:?}"
        );
    }
    assert!(matches!(
        decode(b"\xff\xfe{\"v\":1}"),
        Err(DecodeError::Invalid { .. })
    ));
    // Trailing bytes after the record object are rejected, even valid JSON.
    let mut trailing = leaf_line().to_vec();
    trailing.pop();
    trailing.extend_from_slice(b" {\"v\":1}");
    assert!(matches!(
        decode(&trailing),
        Err(DecodeError::Invalid { .. })
    ));
    let mut second = leaf_line().to_vec();
    second.extend_from_slice(
        b"{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}\n",
    );
    assert!(matches!(decode(&second), Err(DecodeError::Invalid { .. })));
}

#[test]
fn missing_trailing_lf_still_decodes_and_encode_always_adds_one() {
    let mut bare = leaf_line().to_vec();
    bare.pop();
    let decoded = decode(&bare).expect("LF is optional on decode");
    assert!(matches!(decoded.record, Record::Leaf { to: None, .. }));
    let line = encode(&decoded.record).expect("leaf encodes");
    assert_eq!(line, leaf_line());
    assert!(line.ends_with(b"\n"));
    assert!(!line[..line.len() - 1].contains(&b'\n'));
}

#[test]
fn max_record_bound_is_pinned_and_ordinary_lines_fit() {
    assert_eq!(MAX_RECORD, 67_108_864);
    assert_eq!(JOURNAL_VERSION, 2);
    // The codec itself carries no length gate (the fold layer enforces the
    // bound via `encoded_len` when attaching replay payloads): a two-megabyte
    // text part round-trips through `encode`/`decode` untouched.
    let big = "x".repeat(2_000_000);
    let record = user_record(1, None, &big);
    let line = encode(&record).expect("large record encodes");
    let len = u64::try_from(line.len()).expect("test line fits u64");
    assert!(len < MAX_RECORD);
    let back = decode(&line).expect("large record decodes").record;
    assert_eq!(back, record);
}

// ---- VOC-07A: member order and strictness ----

#[test]
fn encode_writes_version_type_and_entry_prefix_first() {
    let line = encode(&Record::Leaf {
        at: stamp(),
        to: None,
    })
    .expect("leaf encodes");
    let text = std::str::from_utf8(&line).expect("utf8");
    let version_at = text.find("\"v\":1").expect("version first");
    let type_at = text.find("\"type\":\"leaf\"").expect("type second");
    let stamp_at = text.find("\"at\":").expect("at member");
    assert!(version_at < type_at && type_at < stamp_at, "{text}");

    let line = encode(&user_record(4, Some(3), "hi")).expect("user encodes");
    let text = std::str::from_utf8(&line).expect("utf8");
    let mut cursor = 0;
    for member in [
        "\"v\":1",
        "\"type\":\"user\"",
        "\"id\":4",
        "\"parent\":3",
        "\"at\":",
    ] {
        let offset = text[cursor..]
            .find(member)
            .unwrap_or_else(|| panic!("{member} in {text}"));
        cursor += offset + member.len();
    }

    let line = encode(&Record::TurnStart {
        at: stamp(),
        turn: tid(1),
    })
    .expect("turn encodes");
    let text = std::str::from_utf8(&line).expect("utf8");
    assert!(text.starts_with("{\"v\":1,\"type\":\"turn_start\",\"at\":"));
}

#[test]
fn unknown_top_level_members_are_ignored() {
    // Forward compatibility: a later format's extra top-level member decodes.
    let line = "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null,\"future\":1}\n";
    let decoded = decode(line.as_bytes()).expect("unknown top-level member ignored");
    assert!(matches!(decoded.record, Record::Leaf { to: None, .. }));
    // Re-encoding drops the unknown member, so bytes are the canonical form.
    assert_eq!(
        encode(&decoded.record).expect("re-encode").as_slice(),
        leaf_line()
    );
}

#[test]
fn unknown_nested_members_are_rejected() {
    // The closed `usage` object rejects even a correctly typed stranger and
    // stays strict about the money spelling.
    let base = "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"stop\":\"done\",\"usage\":{USAGE}}";
    for usage in [
        "{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null,\"future\":1}",
        "{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_usd\":0.5}",
    ] {
        let line = base.replace("USAGE", usage);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    // Duplicate nested members are rejected at the second value.
    let dupe = base.replace(
        "USAGE",
        "{\"input\":1,\"input\":2,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}",
    );
    assert!(matches!(
        decode(dupe.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    // Unknown part members are rejected too.
    let part = "{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{\"type\":\"text\",\"text\":\"a\",\"future\":1}]}";
    assert!(matches!(
        decode(part.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn deeply_nested_opaque_bodies_survive_the_codec() {
    // Extension bodies are opaque raw JSON: 64 levels of nesting round-trip.
    let mut body = String::from("0");
    for _ in 0..64 {
        body = format!("[{body}]");
    }
    let raw = RawJson::parse(&body).expect("nested body parses");
    let record = Record::Ext {
        at: stamp(),
        ext: "probe".into(),
        kind: "nested".into(),
        body: raw,
    };
    let line = encode(&record).expect("nested ext encodes");
    assert_eq!(decode(&line).expect("nested ext decodes").record, record);
}

// ---- VOC-07A: mail, leaf, inferred ----

#[test]
fn mail_mode_is_a_closed_three_tag_set() {
    for (mode, literal) in [
        (MailMode::Aside, "aside"),
        (MailMode::Steer, "steer"),
        (MailMode::NextTurn, "next_turn"),
    ] {
        let record = Record::Mail(Mail {
            at: stamp(),
            from: SessionId::new_v7(),
            to: SessionId::new_v7(),
            mode,
            text: "hello".into(),
            reply_to: None,
        });
        let line = encode(&record).expect("mail encodes");
        let text = std::str::from_utf8(&line).expect("utf8");
        assert!(text.contains(&format!("\"mode\":\"{literal}\"")), "{text}");
        assert_eq!(decode(&line).expect("mail decodes").record, record);
    }
    // Any other string, null, a number, or a missing member is rejected at
    // decode. The encoder cannot spell another mode: `MailMode` is closed.
    let base = "{\"v\":1,\"type\":\"mail\",\"at\":\"2026-09-26T10:15:30.123Z\",\"from\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"to\":\"01927f40-0000-7000-8000-000000000001\",\"text\":\"hi\",\"reply_to\":null,MODE}";
    for mode in ["\"mode\":\"bogus\"", "\"mode\":null", "\"mode\":42"] {
        let line = base.replace("MODE", mode);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    let missing = base.replace(",MODE", "");
    assert!(matches!(
        decode(missing.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn leaf_to_none_is_valid_and_inferred_purpose_is_strict() {
    let record = Record::Leaf {
        at: stamp(),
        to: None,
    };
    let line = encode(&record).expect("leaf encodes");
    let text = std::str::from_utf8(&line).expect("utf8");
    assert!(text.contains("\"to\":null}"), "{text}");
    assert_eq!(decode(&line).expect("leaf decodes").record, record);

    let record = Record::Inferred {
        at: stamp(),
        who: Owner::Extension {
            name: "fusion".into(),
            origin: "bundled".into(),
        },
        purpose: InferredPurpose::Synthetic {
            id: "dalgona/fusion".into(),
        },
        usage: usage(),
    };
    let line = encode(&record).expect("inferred encodes");
    let text = std::str::from_utf8(&line).expect("utf8");
    assert!(
        text.contains("\"purpose\":{\"synthetic\":{\"id\":\"dalgona/fusion\"}}"),
        "{text}"
    );
    assert_eq!(decode(&line).expect("inferred decodes").record, record);

    let base = "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"core\",\"purpose\":PURPOSE,\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}";
    for purpose in [
        "{\"bogus\":{\"id\":\"x\"}}",
        "{\"synthetic\":{}}",
        "{\"synthetic\":{\"id\":42}}",
        "\"core\"",
    ] {
        let line = base.replace("PURPOSE", purpose);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    // `who` is strict too: unknown literals and shapes are rejected.
    let who_line = "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"harness\",\"purpose\":{\"synthetic\":{\"id\":\"x\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}";
    assert!(matches!(
        decode(who_line.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

// ---- VOC-07A: assistant route provenance ----

#[test]
fn absent_route_stays_absent_and_reencodes_without_one() {
    let line = assistant_line("dalgona/fusion", "");
    let decoded = decode(line.as_bytes()).expect("legacy assistant decodes");
    let Record::Assistant(entry) = &decoded.record else {
        panic!("expected assistant");
    };
    let EntryKind::Assistant { route, .. } = &entry.kind else {
        panic!("expected assistant kind");
    };
    assert_eq!(*route, None);
    // An old record re-encodes without adding a route member.
    assert_eq!(
        encode(&decoded.record).expect("re-encode").as_slice(),
        line.as_bytes()
    );
}

#[test]
fn known_routes_round_trip_and_check_the_model_grammar() {
    for (model, route) in [
        ("dalgona/fusion", AssistantRoute::Synthetic),
        ("dalgon/normal", AssistantRoute::Harness),
    ] {
        let record = assistant_record(model, Some(route));
        let line = encode(&record).expect("routed assistant encodes");
        let back = decode(&line).expect("routed assistant decodes").record;
        assert_eq!(back, record);
    }
    let api = assistant_record("anything-at-all", Some(AssistantRoute::Api(Family::Chat)));
    let line = encode(&api).expect("api route encodes");
    assert!(
        std::str::from_utf8(&line)
            .expect("utf8")
            .contains("\"route\":{\"api\":\"openai_chat\"}")
    );
    assert_eq!(decode(&line).expect("api route decodes").record, api);

    // A synthetic route over a non-synthetic id is rejected on decode and encode.
    let bad = assistant_line("BAD ID!", ",\"route\":\"synthetic\"");
    assert!(matches!(
        decode(bad.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    assert!(matches!(
        encode(&assistant_record(
            "BAD ID!",
            Some(AssistantRoute::Synthetic)
        )),
        Err(EncodeError::InvalidRoute(_))
    ));
    // A harness route over a non-harness id is rejected the same way.
    let bad = assistant_line("dalgona/fusion", ",\"route\":\"harness\"");
    assert!(matches!(
        decode(bad.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    assert!(matches!(
        encode(&assistant_record(
            "dalgona/fusion",
            Some(AssistantRoute::Harness)
        )),
        Err(EncodeError::InvalidRoute(_))
    ));
}

#[test]
fn present_null_and_malformed_routes_are_rejected() {
    for route in [
        ",\"route\":null",
        ",\"route\":\"bogus\"",
        ",\"route\":{\"api\":\"bogus\"}",
        ",\"route\":{\"synthetic\":1}",
        ",\"route\":{\"api\":\"openai_chat\",\"extra\":1}",
    ] {
        let line = assistant_line("dalgona/fusion", route);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    // A present null replay is malformed too, like a present null route.
    let line = assistant_line("dalgona/fusion", "")
        .replace("\"stop\":\"done\"", "\"stop\":\"done\",\"replay\":null");
    assert!(matches!(
        decode(line.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn encode_rejects_empty_model_ids() {
    let entry = Entry {
        id: eid(5),
        parent: None,
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "".into(),
            content: Vec::new(),
            usage: usage(),
            stop: AssistantStop::Done,
            route: None,
            replay: None,
        },
    };
    assert!(matches!(
        encode(&Record::Assistant(entry)),
        Err(EncodeError::EmptyModel)
    ));
}

// ---- VOC-07A: resolution causes ----

#[test]
fn resolved_causes_round_trip_and_absent_stays_legacy() {
    let request = RequestId::new_v7();
    // Absent cause is the legacy shape: valid, and re-encodes byte-for-byte.
    let legacy = resolved_line(&request, "");
    let decoded = decode(&legacy).expect("legacy resolved decodes").record;
    let Record::Resolved {
        cause, was_default, ..
    } = &decoded
    else {
        panic!("expected resolved");
    };
    assert_eq!(*cause, None);
    assert!(!was_default);
    assert_eq!(
        encode(&decoded).expect("re-encode").as_slice(),
        legacy.as_slice()
    );

    for (cause, literal) in [
        (ResolutionCause::Answered, "answered"),
        (ResolutionCause::TimedOut, "timed_out"),
        (ResolutionCause::Cancelled, "cancelled"),
        (ResolutionCause::Withdrawn, "withdrawn"),
        (ResolutionCause::NoController, "no_controller"),
    ] {
        let line = resolved_line(&request, &format!(",\"cause\":\"{literal}\""));
        let decoded = decode(&line).expect("caused resolved decodes").record;
        assert!(matches!(decoded, Record::Resolved { cause: Some(found), .. } if found == cause));
        assert_eq!(
            encode(&decoded).expect("re-encode").as_slice(),
            line.as_slice()
        );
    }
    // Present-but-wrong causes are invalid records, not legacy ones.
    for tail in [",\"cause\":null", ",\"cause\":42", ",\"cause\":\"bogus\""] {
        let line = resolved_line(&request, tail);
        assert!(
            matches!(decode(&line), Err(DecodeError::Invalid { .. })),
            "{}",
            String::from_utf8_lossy(&line)
        );
    }
}

// ---- VOC-07A: misc typed boundaries ----

#[test]
fn mismatched_tree_variant_is_refused_not_misencoded() {
    let entry = user_entry(1, None, "hi");
    assert!(matches!(
        encode(&Record::Assistant(entry)),
        Err(EncodeError::MismatchedKind)
    ));
}

#[test]
fn scan_head_labels_v1_tree_prefixes_and_skips_the_rest() {
    let line = encode(&user_record(4, Some(3), "hi")).expect("user encodes");
    let head = scan_head(&line).expect("v1 tree line scans");
    assert_eq!(head.id, eid(4));
    assert_eq!(head.parent, Some(eid(3)));
    let reserved = encode(&Record::TurnReserved {
        at: stamp(),
        turn: tid(2),
    })
    .expect("reserved encodes");
    assert!(
        scan_head(&reserved).is_none(),
        "v2 lines never scan as v1 heads"
    );
    assert!(scan_head(b"{\"v\":1,\"type\":\"bogus\",\"id\":4,\"parent\":3}").is_none());
}

// ---- VOC-07B: branch projection ----

fn chain_fixture() -> (Header, Vec<Record>) {
    let header = header();
    let records = vec![
        user_record(1, None, "first"),
        user_record(2, Some(1), "second"),
        user_record(3, Some(2), "third"),
        Record::Label {
            at: stamp(),
            entry: eid(1),
            label: Some("kept".into()),
        },
        Record::Label {
            at: stamp(),
            entry: eid(3),
            label: Some("dropped".into()),
        },
    ];
    (header, records)
}

fn entry_ids(records: &[Record]) -> Vec<u64> {
    records
        .iter()
        .filter_map(|record| record.entry().map(|entry| entry.id.get()))
        .collect()
}

#[test]
fn fork_copies_the_path_through_the_anchor_parent() {
    let (header, records) = chain_fixture();
    let forked = branch(
        &records,
        Some(eid(3)),
        BranchMode::Fork { at: eid(2) },
        &header,
    )
    .expect("fork works");
    // The path runs root..anchor-parent, so the anchor itself is excluded and
    // its text rides `anchor_parts` for the caller to re-prompt.
    assert_eq!(entry_ids(&forked.records), vec![1]);
    assert_eq!(
        forked.anchor_parts,
        vec![JournalPart::Text {
            text: "second".into()
        }]
    );
    // The on-path label copies; the off-path label does not.
    assert_eq!(forked.records.len(), 2);
    // Entry ids survive and parents still chain along the copied path.
    let Record::User(entry) = &forked.records[0] else {
        panic!("expected user entry");
    };
    assert_eq!((entry.id, entry.parent), (eid(1), None));
    // The destination header names the source session and anchor.
    assert_eq!(
        forked.header.from,
        Some(Source {
            session: header.id,
            entry: Some(eid(2))
        })
    );
}

#[test]
fn clone_copies_the_current_leaf_path() {
    let (header, records) = chain_fixture();
    let cloned = branch(&records, Some(eid(3)), BranchMode::Clone, &header).expect("clone works");
    assert_eq!(entry_ids(&cloned.records), vec![1, 2, 3]);
    assert_eq!(cloned.anchor_parts, [] as [JournalPart; 0]);
    assert_eq!(
        cloned.header.from,
        Some(Source {
            session: header.id,
            entry: Some(eid(3))
        })
    );
}

#[test]
fn clone_without_a_leaf_has_no_path() {
    let (header, records) = chain_fixture();
    assert!(matches!(
        branch(&records, None, BranchMode::Clone, &header),
        Err(BranchError::NoEntries)
    ));
    assert!(matches!(
        branch(&[], None, BranchMode::Clone, &header),
        Err(BranchError::NoEntries)
    ));
}

#[test]
fn fork_rejects_non_user_anchors_and_unknown_ids() {
    let (header, mut records) = chain_fixture();
    records.push(Record::Assistant(Entry {
        id: eid(7),
        parent: Some(eid(3)),
        at: stamp(),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "dalgona/fusion".into(),
            content: Vec::new(),
            usage: usage(),
            stop: AssistantStop::Done,
            route: None,
            replay: None,
        },
    }));
    records.push(Record::ToolResult(Entry {
        id: eid(8),
        parent: Some(eid(7)),
        at: stamp(),
        kind: EntryKind::ToolResult {
            call: CallId::new("call_1"),
            name: "read".into(),
            error: false,
            parts: Vec::new(),
            changes: Vec::new(),
            elapsed_ms: None,
        },
    }));
    for anchor in [7, 8] {
        assert!(matches!(
            branch(&records, Some(eid(3)), BranchMode::Fork { at: eid(anchor) }, &header),
            Err(BranchError::NotUserEntry { entry }) if entry == eid(anchor)
        ));
    }
    assert!(matches!(
        branch(&records, Some(eid(3)), BranchMode::Fork { at: eid(9) }, &header),
        Err(BranchError::UnknownEntry { entry }) if entry == eid(9)
    ));
    assert!(matches!(
        branch(&records, Some(eid(9)), BranchMode::Clone, &header),
        Err(BranchError::UnknownEntry { entry }) if entry == eid(9)
    ));
}

#[test]
fn fork_ignores_the_leaf_and_settings_follow_the_path() {
    let (header, mut records) = chain_fixture();
    // A fork does not consult the leaf at all, even an unknown one.
    let forked = branch(
        &records,
        Some(eid(99)),
        BranchMode::Fork { at: eid(2) },
        &header,
    )
    .expect("fork ignores the leaf");
    assert_eq!(entry_ids(&forked.records), vec![1]);

    // Path-local settings entries copy with the path; off-path ones drop.
    records.push(Record::Model(Entry {
        id: eid(1),
        parent: None,
        at: stamp(),
        kind: EntryKind::Model {
            route: ModelRoute::synthetic("dalgona/fusion").expect("valid route"),
        },
    }));
    let forked = branch(
        &records,
        Some(eid(3)),
        BranchMode::Fork { at: eid(2) },
        &header,
    )
    .expect("fork with settings");
    assert!(
        forked
            .records
            .iter()
            .any(|record| matches!(record, Record::Model(entry) if entry.id == eid(1))),
        "on-path settings copy"
    );
}

#[test]
fn dangling_parents_terminate_without_panic() {
    let (header, _) = chain_fixture();
    let records = vec![user_record(7, Some(99), "orphan")];
    let cloned = branch(&records, Some(eid(7)), BranchMode::Clone, &header)
        .expect("dangling parent terminates");
    assert_eq!(entry_ids(&cloned.records), vec![7]);
}

#[test]
fn cyclic_parents_terminate_without_hanging() {
    for records in [
        vec![user_record(5, Some(5), "self-parent")],
        vec![
            user_record(5, Some(6), "cycle-a"),
            user_record(6, Some(5), "cycle-b"),
        ],
    ] {
        let header = header();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let out = branch(&records, Some(eid(5)), BranchMode::Clone, &header);
            let _ = done_tx.send(out.is_ok());
        });
        // A parent cycle must not hang the projection: ten seconds is far
        // beyond scheduling noise for a walk over two entries.
        let finished = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("branch must terminate on cyclic parents");
        assert!(finished, "cyclic input still projects");
    }
}

// ---- VOC-08A: curated public exports ----

fn pin_type<T>() {
    let _ = std::mem::size_of::<T>();
}

fn pin_fn<T>(_: T) {}

/// Compile-time pin of the command/config/content export surface: every item
/// these modules re-export through `dal_core::` is named here, so a rename or
/// removal breaks this build.
#[test]
fn vocabulary_surface_commands_config_content() {
    use dal_core::{
        AgentsConfig, ApprovalMode, AskConfig, AskTimeoutClamp, BusyState, CancelScope, Chooser,
        ClampSide, Classify, Command, CommandError, Completion, Config, ConfigError,
        ConfigOverrides, ConfigProduct, ContentError, ContentLimits, EditStyleInput, ErrorTriple,
        Expect, ExportFormat, FrontAction, GuardBands, GuardCheckMode, GuardPolicies, GuardSection,
        ImportFailure, JudgeMode, LexError, Mode, ModeProjection, Output, PROMPT_DIAGRAMS, Part,
        PluginLimits, Rejection, Reply, RulesConfig, Save, Screen, ServeConfig, TuiConfig,
    };
    pin_type::<AgentsConfig>();
    pin_type::<ApprovalMode>();
    pin_type::<AskConfig>();
    pin_type::<AskTimeoutClamp>();
    pin_type::<BusyState>();
    pin_type::<CancelScope>();
    pin_type::<Chooser>();
    pin_type::<ClampSide>();
    pin_type::<Classify>();
    pin_type::<Command>();
    pin_type::<CommandError>();
    pin_type::<Completion>();
    pin_type::<Config>();
    pin_type::<ConfigError>();
    pin_type::<ConfigOverrides>();
    pin_type::<ConfigProduct>();
    pin_type::<ContentError>();
    pin_type::<ContentLimits>();
    pin_type::<EditStyleInput>();
    pin_type::<ErrorTriple>();
    pin_type::<Expect>();
    pin_type::<ExportFormat>();
    pin_type::<FrontAction>();
    pin_type::<GuardBands>();
    pin_type::<GuardCheckMode>();
    pin_type::<GuardPolicies>();
    pin_type::<GuardSection>();
    pin_type::<ImportFailure>();
    pin_type::<JudgeMode>();
    pin_type::<LexError>();
    pin_type::<Mode>();
    pin_type::<ModeProjection>();
    pin_type::<Output>();
    pin_type::<Part>();
    pin_type::<PluginLimits>();
    pin_type::<Rejection>();
    pin_type::<Reply>();
    pin_type::<RulesConfig>();
    pin_type::<Save>();
    pin_type::<Screen>();
    pin_type::<ServeConfig>();
    pin_type::<TuiConfig>();
    pin_fn(dal_core::classify);
    pin_fn(dal_core::tokens);
    let _: &str = PROMPT_DIAGRAMS;
}

/// Compile-time pin of the id/journal export surface.
#[test]
fn vocabulary_surface_ids_and_journal() {
    use dal_core::{
        AssistantRoute, AssistantStop, BlobId, Block, Branch, BranchError, BranchMode, CallId,
        ClientId, DecodeError, Decoded, EncodeError, Entry, EntryId, EntryKind, Gen, GenerationId,
        Header, IdError, InferredPurpose, JobEvent, JobId, JobKind, JobOutcome, JournalPart, Mail,
        PrivateEffectId, PrivateLoopId, PrivateOutcome, Product, Record, RequestId, ScannedHead,
        Seq, SessionId, Source, TreeKind, TurnEndStop, TurnId,
    };
    pin_type::<AssistantRoute>();
    pin_type::<AssistantStop>();
    pin_type::<Block>();
    pin_type::<BlobId>();
    pin_type::<Branch>();
    pin_type::<BranchError>();
    pin_type::<BranchMode>();
    pin_type::<CallId>();
    pin_type::<ClientId>();
    pin_type::<DecodeError>();
    pin_type::<Decoded>();
    pin_type::<EncodeError>();
    pin_type::<Entry>();
    pin_type::<EntryId>();
    pin_type::<EntryKind>();
    pin_type::<Gen>();
    pin_type::<GenerationId>();
    pin_type::<Header>();
    pin_type::<IdError>();
    pin_type::<InferredPurpose>();
    pin_type::<JobEvent>();
    pin_type::<JobId>();
    pin_type::<JobKind>();
    pin_type::<JobOutcome>();
    pin_type::<JournalPart>();
    pin_type::<Mail>();
    pin_type::<PrivateEffectId>();
    pin_type::<PrivateLoopId>();
    pin_type::<PrivateOutcome>();
    pin_type::<Product>();
    pin_type::<Record>();
    pin_type::<RequestId>();
    pin_type::<ScannedHead>();
    pin_type::<Seq>();
    pin_type::<SessionId>();
    pin_type::<Source>();
    pin_type::<TreeKind>();
    pin_type::<TurnEndStop>();
    pin_type::<TurnId>();
    pin_fn(dal_core::branch);
    pin_fn(dal_core::decode);
    pin_fn(dal_core::encode);
    pin_fn(dal_core::scan_head);
    pin_fn(dal_core::resume_token);
    let _: u16 = JOURNAL_VERSION;
    let _: u64 = MAX_RECORD;
    let _: &str = BEFORE_TURN_SOURCE;
}

/// Compile-time pin of the model/raw/request/tokens export surface.
#[test]
fn vocabulary_surface_model_raw_request_tokens() {
    use dal_core::{
        Answer, AnswerValue, AssistantPart, CallGrant, Caps, Choice, CompactOutcome,
        CompactedHistory, ContextItem, Family, InferFailure, Inference, JobEnd,
        MAX_SYNTHETIC_DEPTH, ModelInfo, ModelPrice, ModelRequest, ModelRoute, ModelToolSpec, Owner,
        Preview, PriceTier, Purpose, Question, RawJson, RawJsonError, ReplayCapture, ReplayRefusal,
        ReplaySource, Request, RequestParams, RouteError, Stop, StreamChannel, StreamEvent,
        ThinkingLevel, Usage, check_synthetic_chain, estimate_text_tokens, estimate_tokens,
    };
    pin_type::<Answer>();
    pin_type::<AnswerValue>();
    pin_type::<AssistantPart>();
    pin_type::<CallGrant>();
    pin_type::<Caps>();
    pin_type::<Choice>();
    pin_type::<CompactOutcome>();
    pin_type::<CompactedHistory>();
    pin_type::<ContextItem>();
    pin_type::<Family>();
    pin_type::<InferFailure>();
    pin_type::<Inference>();
    pin_type::<JobEnd>();
    pin_type::<ModelInfo>();
    pin_type::<ModelPrice>();
    pin_type::<ModelRequest>();
    pin_type::<ModelRoute>();
    pin_type::<ModelToolSpec>();
    pin_type::<Owner>();
    pin_type::<Preview>();
    pin_type::<PriceTier>();
    pin_type::<Purpose>();
    pin_type::<Question>();
    pin_type::<RawJson>();
    pin_type::<RawJsonError>();
    pin_type::<ReplayCapture>();
    pin_type::<ReplayRefusal>();
    pin_type::<ReplaySource>();
    pin_type::<Request>();
    pin_type::<RequestParams>();
    pin_type::<RouteError>();
    pin_type::<Stop>();
    pin_type::<StreamChannel>();
    pin_type::<StreamEvent>();
    pin_type::<ThinkingLevel>();
    pin_type::<Usage>();
    pin_fn(check_synthetic_chain);
    pin_fn(estimate_text_tokens);
    pin_fn(estimate_tokens);
    let _: usize = MAX_SYNTHETIC_DEPTH;
}

/// Compile-time pin of the update/view/workspace/terminal export surface.
#[test]
fn vocabulary_surface_update_view_workspace_terminal() {
    use dal_core::{
        AutoCompaction, EntryView, ExtState, ExtStatus, FileChange, ListQuery, Notice, Page,
        PageReq, PageReqError, ResolutionCause, SessionInfo, SessionSummary, SettingsView, Stats,
        ToolOutcomeView, TreeBranch, TreeDelta, TreeOutline, TurnCause, TurnState, Update,
        UpdateKind, UsageView, View, Workspace, WorkspaceError, terminal,
    };
    pin_type::<AutoCompaction>();
    pin_type::<EntryView>();
    pin_type::<ExtState>();
    pin_type::<ExtStatus>();
    pin_type::<FileChange>();
    pin_type::<ListQuery>();
    pin_type::<Notice>();
    pin_type::<Page<EntryView>>();
    pin_type::<PageReq>();
    pin_type::<PageReqError>();
    pin_type::<ResolutionCause>();
    pin_type::<SessionInfo>();
    pin_type::<SessionSummary>();
    pin_type::<SettingsView>();
    pin_type::<Stats>();
    pin_type::<ToolOutcomeView>();
    pin_type::<TreeBranch>();
    pin_type::<TreeDelta>();
    pin_type::<TreeOutline>();
    pin_type::<TurnCause>();
    pin_type::<TurnState>();
    pin_type::<Update>();
    pin_type::<UpdateKind>();
    pin_type::<UsageView>();
    pin_type::<View>();
    pin_type::<Workspace>();
    pin_type::<WorkspaceError>();
    let _ = terminal::escape("x");
    let _ = terminal::escape_preserving_newlines("x\ny");
    let mut pushed = String::new();
    terminal::push_escaped(&mut pushed, 'a');
    assert_eq!(pushed, "a");
}

#[test]
fn lib_rs_carries_no_glob_reexports() {
    const LIB_RS: &str = include_str!("../src/lib.rs");
    assert!(!LIB_RS.contains("::*"), "glob re-exports are forbidden");
    assert!(
        !LIB_RS.contains("pub use *"),
        "glob re-exports are forbidden"
    );
}
