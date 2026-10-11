use super::*;

/// Every canonical vector whose members are spec-shaped. The
/// `req_0142`/`job_0091` lines are shorthand the document uses for
/// readability; the typed members are `UUIDv7` text, so those shapes
/// round-trip in `resolved_and_job_records_round_trip` with real ids.
/// POSIX-only: the vectors carry POSIX workspace paths (see the round-trip
/// test for why).
#[cfg(unix)]
const VECTORS: &[&str] = &[
    "{\"v\":1,\"type\":\"session\",\"id\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"at\":\"2026-09-25T10:15:30.123Z\",\"workspace\":\"/home/alpha/harness/reed\",\"product\":\"dal\",\"from\":null}\n",
    "{\"v\":1,\"type\":\"session\",\"id\":\"01927f40-0000-7000-8000-000000000001\",\"at\":\"2026-09-25T11:00:00.000Z\",\"workspace\":\"/home/alpha/harness/reed\",\"product\":\"dal\",\"from\":{\"session\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"entry\":41}}\n",
    "{\"v\":1,\"type\":\"boot\",\"at\":\"2026-09-25T10:15:30.124Z\",\"gen\":1,\"version\":\"0.1.0\"}\n",
    "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}\n",
    "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":null}\n",
    "{\"v\":1,\"type\":\"label\",\"at\":\"2026-09-25T14:06:00.000Z\",\"entry\":41,\"label\":\"before refactor\"}\n",
    "{\"v\":1,\"type\":\"label\",\"at\":\"2026-09-25T14:06:30.000Z\",\"entry\":41,\"label\":null}\n",
    "{\"v\":1,\"type\":\"name\",\"at\":\"2026-09-25T14:07:00.000Z\",\"name\":\"parser fix\"}\n",
    "{\"v\":1,\"type\":\"name\",\"at\":\"2026-09-25T14:07:30.000Z\",\"name\":null}\n",
    "{\"v\":1,\"type\":\"archive\",\"at\":\"2026-09-25T14:08:00.000Z\",\"archived\":true}\n",
    "{\"v\":1,\"type\":\"turn_start\",\"at\":\"2026-09-25T10:15:31.000Z\",\"turn\":1}\n",
    "{\"v\":1,\"type\":\"tool_start\",\"at\":\"2026-09-25T10:15:35.420Z\",\"turn\":1,\"call\":\"toolu_01\"}\n",
    "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":5400,\"output\":610,\"cache_read\":4200,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":18830},\"changes\":[{\"path\":\"lib/lexer.ml\",\"added\":12,\"removed\":3}]}\n",
    "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"aborted\",\"usage\":{\"input\":5400,\"output\":10,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":310},\"changes\":[]}\n",
    "{\"v\":1,\"type\":\"rule_fired\",\"at\":\"2026-09-25T10:15:40.001Z\",\"turn\":1,\"rule\":\"no-sleep\",\"entry\":7}\n",
    "{\"v\":1,\"type\":\"allow_always\",\"at\":\"2026-09-25T10:15:38.000Z\",\"tool\":\"exec\",\"by\":\"tui\"}\n",
    "{\"v\":1,\"type\":\"grant_given\",\"at\":\"2026-09-25T10:15:39.000Z\",\"ext\":\"web\",\"set\":[\"net\"],\"scope\":\"saved\",\"by\":\"tui\"}\n",
    "{\"v\":1,\"type\":\"grant_given\",\"at\":\"2026-09-25T10:15:39.500Z\",\"ext\":\"orchestration\",\"set\":[\"agents\",\"jobs\"],\"scope\":\"call\",\"by\":\"tui\"}\n",
    "{\"v\":1,\"type\":\"tool_promoted\",\"at\":\"2026-09-25T10:15:41.000Z\",\"tool\":\"write\"}\n",
    "{\"v\":1,\"type\":\"ext\",\"at\":\"2026-09-25T10:15:43.000Z\",\"ext\":\"orchestration\",\"kind\":\"children\",\"body\":{\"run\":\"r1\",\"step\":\"scan\"}}\n",
    "{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":3,\"at\":\"2026-09-25T10:15:31.001Z\",\"parts\":[{\"type\":\"text\",\"text\":\"Fix the parser\"},{\"type\":\"image\",\"mime\":\"image/png\",\"blob\":\"9f2ca1bd...64 hex...\",\"bytes\":48213}]}\n",
    "{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",\"api\":\"openai_responses\",\"model\":\"gpt-6-luna\"}\n",
    "{\"v\":1,\"type\":\"thinking\",\"id\":2,\"parent\":1,\"at\":\"2026-09-25T10:15:30.126Z\",\"level\":\"high\"}\n",
    "{\"v\":1,\"type\":\"approval\",\"id\":3,\"parent\":2,\"at\":\"2026-09-25T10:15:30.127Z\",\"mode\":\"ask\"}\n",
    "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":4,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"claude-opus-5\",\"content\":[{\"type\":\"reasoning\",\"text\":\"The lexer drops the last token.\",\"replay\":{\"type\":\"thinking\",\"thinking\":\"The lexer drops the last token.\",\"signature\":\"EqQBCkgI...\"}},{\"type\":\"text\",\"text\":\"I will read the lexer.\",\"replay\":null},{\"type\":\"tool_call\",\"id\":\"toolu_01\",\"name\":\"read\",\"input\":{\"path\":\"lib/lexer.ml\"}}],\"usage\":{\"input\":1200,\"output\":85,\"cache_read\":0,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":4210},\"stop\":\"tool_use\"}\n",
    "{\"v\":1,\"type\":\"tool_result\",\"id\":6,\"parent\":5,\"at\":\"2026-09-25T10:15:35.431Z\",\"call\":\"toolu_01\",\"name\":\"read\",\"error\":false,\"parts\":[{\"type\":\"text\",\"blob\":\"4be1f00d...64 hex...\",\"bytes\":52110}],\"changes\":[]}\n",
    "{\"v\":1,\"type\":\"reminder\",\"id\":7,\"parent\":6,\"at\":\"2026-09-25T10:15:40.000Z\",\"source\":\"rule:no-sleep\",\"text\":\"Do not add sleep calls to tests.\"}\n",
    "{\"v\":1,\"type\":\"compaction\",\"id\":58,\"parent\":57,\"at\":\"2026-09-25T13:00:00.000Z\",\"summary\":\"The user asked to fix the parser...\",\"first_kept\":51,\"tokens_before\":171234,\"replay\":null,\"usage\":{\"input\":5400,\"output\":610,\"cache_read\":4200,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":18830}}\n",
    "{\"v\":1,\"type\":\"compaction\",\"id\":90,\"parent\":89,\"at\":\"2026-09-25T14:00:00.000Z\",\"summary\":null,\"first_kept\":null,\"tokens_before\":203004,\"replay\":{\"api\":\"openai_responses\",\"items\":[{\"type\":\"compaction\",\"id\":\"cmp_001\",\"encrypted_content\":\"gAAAAABpM0Yj...\"}]},\"usage\":null}\n",
    "{\"v\":1,\"type\":\"branch_summary\",\"id\":91,\"parent\":41,\"at\":\"2026-09-25T14:05:01.000Z\",\"from\":90,\"summary\":\"On that branch the user tried a table-driven lexer.\"}\n",
    "{\"v\":1,\"type\":\"mail\",\"at\":\"2026-09-26T10:15:30.123Z\",\"from\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"to\":\"01927f40-0000-7000-8000-000000000001\",\"mode\":\"aside\",\"text\":\"review this result\",\"reply_to\":null}\n",
    "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"fusion\",\"origin\":\"bundled\"}},\"purpose\":{\"synthetic\":{\"id\":\"dalgona/fusion\"}},\"usage\":{\"input\":10,\"output\":4,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}\n",
];

fn nz(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(id) => id,
        None => NonZeroU64::MIN,
    }
}

fn user_entry(id: u64, parent: Option<u64>, text: &str) -> Entry {
    Entry {
        id: EntryId::new(nz(id)),
        parent: parent.map(|value| EntryId::new(nz(value))),
        at: "2026-09-25T10:15:30.000Z"
            .parse()
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    }
}

/// A host-absolute workspace for tests that only need one that parses.
fn harness_workspace() -> Result<Workspace, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    const ROOT: &str = "/home/alpha/harness/reed";
    #[cfg(windows)]
    const ROOT: &str = "C:\\home\\alpha\\harness\\reed";
    Ok(Workspace::new(ROOT.into())?)
}

// The canonical vectors carry POSIX workspace paths; decode-side workspace
// validation is host-coupled, so the byte-for-byte contract only runs where
// those paths parse as absolute.
#[cfg(unix)]
#[test]
fn canonical_vectors_round_trip_byte_for_byte() -> Result<(), Box<dyn std::error::Error>> {
    for line in VECTORS {
        let decoded = decode(line.as_bytes())?;
        let encoded = encode(&decoded.record)?;
        assert_eq!(
            encoded.as_slice(),
            line.as_bytes(),
            "round trip diverged for {line}"
        );
    }
    Ok(())
}

#[test]
fn resolved_and_extended_records_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    // The doc vectors use `req_0142`/`job_0091` shorthand; the typed
    // members are UUIDv7 text, so these shapes ride real ids.
    let lines = [
        "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:36.000Z\",\"request\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"answer\":\"approve\",\"by\":\"tui\"}\n",
        "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:37.000Z\",\"request\":\"01927f40-0000-7000-8000-000000000001\",\"answer\":{\"value\":{\"text\":\"alpha\"}},\"by\":\"rpc:7:codex-desktop\"}\n",
        "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:15:42.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"start\",\"kind\":\"worker\"}\n",
        "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:00.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"end\",\"outcome\":{\"exited\":0}}\n",
        "{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:01.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"orphaned\"}\n",
        "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:38.000Z\",\"request\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"answer\":\"approve\",\"by\":\"tui\",\"was_default\":true}\n",
        "{\"v\":1,\"type\":\"tool_promoted\",\"at\":\"2026-09-25T10:15:41.000Z\",\"tool\":\"write\",\"turn\":1,\"leaf\":41}\n",
        "{\"v\":1,\"type\":\"scoped_grant\",\"at\":\"2026-09-25T10:15:39.000Z\",\"call\":\"call_1\",\"prefix\":[\"git\",\"push\"],\"roots\":[\"/work\"],\"job\":\"01927f40-0000-7000-8000-000000000002\",\"by\":\"tui\"}\n",
        "{\"v\":1,\"type\":\"scoped_grant_ended\",\"at\":\"2026-09-25T10:16:40.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\"}\n",
        "{\"v\":1,\"type\":\"before_request_mut\",\"at\":\"2026-09-25T10:15:33.000Z\",\"turn\":1,\"ext\":\"redact\",\"field\":\"model\",\"old\":\"a\",\"new\":\"b\"}\n",
        "{\"v\":1,\"type\":\"wake_attempt\",\"at\":\"2026-09-25T10:15:34.000Z\",\"turn\":1,\"count\":2}\n",
    ];
    for line in lines {
        let decoded = decode(line.as_bytes())?;
        assert_eq!(
            encode(&decoded.record)?.as_slice(),
            line.as_bytes(),
            "round trip diverged for {line}"
        );
    }
    Ok(())
}

#[test]
fn decode_drops_unknown_members_and_rejects_bad_shapes() {
    // A record member the format does not declare is dropped, per the
    // store's `unknown_member_dropped_on_decode` test contract.
    let decoded = decode(
        b"{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41,\"mystery\":true}",
    );
    assert!(matches!(
        decoded,
        Ok(Decoded {
            record: Record::Leaf { .. }
        })
    ));
    assert!(matches!(
        decode(b"{\"v\":2,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}"),
        Err(DecodeError::UnsupportedVersion { found: 2 })
    ));
    assert!(matches!(
        decode(b"{\"type\":\"leaf\",\"at\":\"x\",\"to\":1}"),
        Err(DecodeError::MissingVersion)
    ));
    assert!(matches!(
        decode(b"{\"v\":1,\"type\":\"bogus\",\"at\":\"x\"}"),
        Err(DecodeError::UnknownRecordKind { .. })
    ));
    // The closed `usage` object stays strict: a `cost_usd` member
    // would silently mis-spell money if it were tolerated.
    assert!(matches!(
        decode(b"{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_usd\":0.5},\"stop\":\"done\"}"),
        Err(DecodeError::Invalid { .. })
    ));
    // A non-null `from` object must carry both `session` and `entry`;
    // `entry:null` is the only way to say "no anchor entry".
    assert!(matches!(
        decode(b"{\"v\":1,\"type\":\"session\",\"id\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"at\":\"2026-09-25T10:15:30.123Z\",\"workspace\":\"/w\",\"product\":\"dal\",\"from\":{\"session\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\"}}"),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn decode_rejects_duplicate_version_type_and_record_members_at_second_value() {
    let cases = [
        (
            "{\"v\":1,\"v\":2,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}",
            "\"v\":2",
            4,
        ),
        (
            "{\"v\":1,\"type\":\"leaf\",\"type\":\"name\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}",
            "\"type\":\"name\"",
            7,
        ),
        (
            "{\"v\":1,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41,\"to\":42}",
            "\"to\":42",
            5,
        ),
    ];
    for (line, marker, value_offset) in cases {
        let expected_offset = line.find(marker).unwrap() + value_offset;
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
        ));
    }
}

#[test]
fn decode_rejects_duplicate_nested_members_at_second_value() {
    let line = "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"input\":2,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":0},\"changes\":[]}";
    let expected_offset = line.find("\"input\":2").unwrap() + 8;
    assert!(matches!(
        decode(line.as_bytes()),
        Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
    ));
}

#[test]
fn decode_reports_absolute_nested_member_offsets() {
    let cases = [
        (
            "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"input\":2,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":0},\"changes\":[]}",
            "\"input\":",
            1,
            8,
        ),
        (
            "{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{\"type\":\"text\",\"text\":\"a\",\"text\":\"b\"}]}",
            "\"text\":",
            1,
            7,
        ),
        (
            "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"text\":\"a\",\"text\":\"b\"}],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}",
            "\"text\":",
            1,
            7,
        ),
        (
            "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"x\",\"name\":\"y\",\"origin\":\"bundled\"}},\"purpose\":{\"synthetic\":{\"id\":\"m\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
            "\"name\":",
            1,
            7,
        ),
        (
            "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"core\",\"purpose\":{\"synthetic\":{\"id\":\"x\",\"id\":\"y\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
            "\"id\":",
            1,
            5,
        ),
        (
            "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"future\":2},\"changes\":[]}",
            "\"future\":",
            0,
            9,
        ),
        (
            "{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{\"type\":\"text\",\"future\":1}]}",
            "\"future\":",
            0,
            9,
        ),
        (
            "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[{\"type\":\"text\",\"future\":1}],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}",
            "\"future\":",
            0,
            9,
        ),
        (
            "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":{\"extension\":{\"name\":\"x\",\"origin\":\"bundled\",\"future\":1}},\"purpose\":{\"synthetic\":{\"id\":\"m\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
            "\"future\":",
            0,
            9,
        ),
        (
            "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-26T10:15:31.123Z\",\"who\":\"core\",\"purpose\":{\"synthetic\":{\"id\":\"m\",\"future\":1}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}",
            "\"future\":",
            0,
            9,
        ),
    ];
    for (line, marker, occurrence, value_offset) in cases {
        let marker_at = line.match_indices(marker).nth(occurrence).unwrap().0;
        let expected_offset = marker_at + value_offset;
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
        ));
    }
}

#[test]
fn unsupported_version_precedes_duplicate_type_and_payload() {
    for line in [
        "{\"v\":2,\"type\":\"leaf\",\"type\":\"other\",\"at\":\"x\",\"to\":1}",
        "{\"type\":\"leaf\",\"to\":1,\"to\":2,\"v\":2}",
        "{\"to\":1,\"v\":2,\"type\":\"leaf\",\"to\":2}",
    ] {
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::UnsupportedVersion { found: 2 })
        ));
    }
}

fn turn_end_with_cost(cost_usd: f64) -> Record {
    Record::TurnEnd {
        at: "2026-09-25T10:16:02.000Z"
            .parse()
            .unwrap_or(jiff::Timestamp::UNIX_EPOCH),
        turn: TurnId::new(nz(1)),
        stop: TurnEndStop::Done,
        usage: Some(Usage {
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: Some(cost_usd),
        }),
        changes: Vec::new(),
    }
}

#[test]
fn encode_preserves_representable_high_micro_dollar_cost() -> Result<(), EncodeError> {
    let encoded = encode(&turn_end_with_cost(18_000_000_000_000.0))?;
    assert!(
        std::str::from_utf8(&encoded)
            .is_ok_and(|line| line.contains("\"cost_micro_usd\":18000000000000000000"))
    );
    Ok(())
}

#[test]
fn encode_rejects_cost_rounding_to_u64_overflow() {
    #[expect(
        clippy::cast_precision_loss,
        reason = "u64::MAX rounds to 2^64 as f64, the exclusive micro-dollar bound"
    )]
    let cost_usd = (u64::MAX as f64) / 1_000_000.0;
    assert!(matches!(
        encode(&turn_end_with_cost(cost_usd)),
        Err(EncodeError::InvalidCost)
    ));
}

#[test]
fn encode_rejects_negative_and_infinite_costs() {
    assert!(matches!(
        encode(&turn_end_with_cost(-0.000_001)),
        Err(EncodeError::InvalidCost)
    ));
    assert!(matches!(
        encode(&turn_end_with_cost(f64::INFINITY)),
        Err(EncodeError::InvalidCost)
    ));
}

#[test]
fn encode_keeps_explicit_zero_cost() -> Result<(), EncodeError> {
    let encoded = encode(&turn_end_with_cost(0.0))?;
    assert!(std::str::from_utf8(&encoded).is_ok_and(|line| line.contains("\"cost_micro_usd\":0")));
    Ok(())
}

fn model_line(tail: &str) -> String {
    format!(
        "{{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",{tail}}}\n"
    )
}

fn decoded_route(line: &str) -> Result<ModelRoute, Box<dyn std::error::Error>> {
    let Record::Model(Entry {
        kind: EntryKind::Model { route },
        ..
    }) = decode(line.as_bytes())?.record
    else {
        return Err("expected a model record".into());
    };
    Ok(route)
}

#[test]
fn mode_record_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let entry = Entry {
        id: EntryId::new(nz(1)),
        parent: None,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::Mode {
            mode: Mode::EvalFirst,
        },
    };
    let encoded = encode(&Record::Mode(entry))?;
    let text = std::str::from_utf8(&encoded)?;
    assert!(text.contains(r#""type":"mode""#));
    assert!(text.contains(r#""mode":"eval-first""#));
    let decoded = decode(&encoded)?;
    assert_eq!(
        decoded.record,
        Record::Mode(Entry {
            id: EntryId::new(nz(1)),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Mode {
                mode: Mode::EvalFirst,
            },
        })
    );
    assert_eq!(decoded.record.tag(), "mode");
    let bad =
        r#"{"v":1,"type":"mode","id":1,"parent":null,"at":"1970-01-01T00:00:00Z","mode":"loud"}"#;
    assert!(decode(bad.as_bytes()).is_err());
    Ok(())
}

#[test]
fn model_record_keeps_format1_api_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let line = model_line(r#""api":"openai_responses","model":"gpt-6-luna""#);
    let decoded = decode(line.as_bytes())?;
    let Record::Model(entry) = &decoded.record else {
        return Err("expected a model record".into());
    };
    assert_eq!(
        entry.kind,
        EntryKind::Model {
            route: ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-6-luna".into(),
            },
        }
    );
    assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
    Ok(())
}

#[test]
fn model_record_round_trips_synthetic_and_harness_routes() -> Result<(), Box<dyn std::error::Error>>
{
    let cases = [
        (
            r#""route":{"synthetic":{"id":"dalgona/fusion-2.1_x"}}"#,
            ModelRoute::Synthetic {
                id: "dalgona/fusion-2.1_x".into(),
            },
        ),
        (
            r#""route":{"harness":{"id":"dalgon/eval-first"}}"#,
            ModelRoute::Harness {
                id: "dalgon/eval-first".into(),
            },
        ),
    ];
    for (tail, expected) in cases {
        let line = model_line(tail);
        assert_eq!(decoded_route(&line)?, expected, "{line}");
        assert_eq!(
            encode(&decode(line.as_bytes())?.record)?.as_slice(),
            line.as_bytes()
        );
    }
    Ok(())
}

#[test]
fn model_record_rejects_bad_route_shapes() {
    let cases = [
        // Both spellings at once.
        r#""route":{"synthetic":{"id":"a/b"}},"api":"anthropic","model":"m""#,
        r#""model":"m","route":{"harness":{"id":"dalgon/normal"}}"#,
        // Neither spelling, or half of the API pair.
        r#""name":"m""#,
        r#""model":"m""#,
        r#""api":"anthropic""#,
        // An unknown family stays rejected.
        r#""api":"openai_other","model":"m""#,
        // No tag, two tags, an API tag, or a non-object route.
        r#""route":{}"#,
        r#""route":{"synthetic":{"id":"a/b"},"harness":{"id":"dalgon/normal"}}"#,
        r#""route":{"api":{"family":"anthropic","model":"m"}}"#,
        r#""route":"dalgon/normal""#,
        // Missing, unknown, duplicate, or non-string fields.
        r#""route":{"synthetic":{}}"#,
        r#""route":{"synthetic":{"id":"a/b","family":"anthropic"}}"#,
        r#""route":{"harness":{"id":"dalgon/normal","id":"dalgon/eval-only"}}"#,
        r#""route":{"synthetic":{"id":1}}"#,
        // A duplicate record member.
        r#""route":{"harness":{"id":"dalgon/normal"}},"route":{"harness":{"id":"dalgon/normal"}}"#,
        // Ids outside the closed grammars.
        r#""route":{"synthetic":{"id":"Upper/x"}}"#,
        r#""route":{"synthetic":{"id":"no-slash"}}"#,
        r#""route":{"harness":{"id":"dalgon/fast"}}"#,
        r#""route":{"harness":{"id":"dalgona/fusion"}}"#,
    ];
    for tail in cases {
        let line = model_line(tail);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    let line = model_line(r#""route":{"harness":{"id":"dalgon/fast"}}"#);
    let expected = line.find("\"dalgon/fast\"").unwrap_or(usize::MAX);
    assert!(matches!(
        decode(line.as_bytes()),
        Err(DecodeError::Invalid { offset, .. }) if offset == expected
    ));
}

#[test]
fn encode_rejects_non_api_route_the_decoder_would_reject() {
    let entry = |route: ModelRoute| {
        Record::Model(Entry {
            id: EntryId::new(nz(1)),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Model { route },
        })
    };
    assert!(matches!(
        encode(&entry(ModelRoute::Synthetic { id: "bad".into() })),
        Err(EncodeError::InvalidRoute(
            RouteError::InvalidSyntheticId { .. }
        ))
    ));
    assert!(matches!(
        encode(&entry(ModelRoute::Harness {
            id: "dalgon/fast".into()
        })),
        Err(EncodeError::InvalidRoute(
            RouteError::InvalidHarnessId { .. }
        ))
    ));
}

#[test]
fn empty_model_id_is_rejected_both_ways() -> Result<(), Box<dyn std::error::Error>> {
    let assistant = "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"done\"}\n";
    let empty = assistant.replace("\"model\":\"m\"", "\"model\":\"\"");
    let expected = empty.find("\"model\":\"\"").map(|at| at + 8);
    assert!(matches!(
        decode(empty.as_bytes()),
        Err(DecodeError::Invalid { offset, .. }) if Some(offset) == expected
    ));
    assert!(matches!(
        decode(model_line(r#""api":"anthropic","model":"""#).as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    let mut record = decode(assistant.as_bytes())?.record;
    assert_eq!(encode(&record)?.as_slice(), assistant.as_bytes());
    if let Record::Assistant(Entry {
        kind: EntryKind::Assistant { model, .. },
        ..
    }) = &mut record
    {
        *model = "".into();
    }
    // The serde path (views, updates) refuses the same empty id: the
    // derived spelling of this record differs from the valid one only
    // in `model`, and the valid one decodes.
    let Record::Assistant(entry) = &record else {
        return Err("expected an assistant record".into());
    };
    let serde_text = sonic_rs::to_string(&entry.kind)?;
    assert!(sonic_rs::from_str::<EntryKind>(&serde_text).is_err());
    let valid = serde_text.replace("\"model\":\"\"", "\"model\":\"m\"");
    sonic_rs::from_str::<EntryKind>(&valid)?;
    assert!(matches!(encode(&record), Err(EncodeError::EmptyModel)));
    let api = Record::Model(Entry {
        id: EntryId::new(nz(1)),
        parent: None,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::Model {
            route: ModelRoute::Api {
                family: Family::Anthropic,
                model: "".into(),
            },
        },
    });
    assert!(matches!(encode(&api), Err(EncodeError::EmptyModel)));
    let Record::Model(entry) = &api else {
        return Err("expected a model record".into());
    };
    let serde_text = sonic_rs::to_string(&entry.kind)?;
    assert!(sonic_rs::from_str::<EntryKind>(&serde_text).is_err());
    let valid = serde_text.replace("\"model\":\"\"", "\"model\":\"m\"");
    sonic_rs::from_str::<EntryKind>(&valid)?;
    Ok(())
}

#[test]
fn generic_blob_part_round_trips_and_stays_strict() -> Result<(), Box<dyn std::error::Error>> {
    let user_line = |part: &str| {
        format!(
            "{{\"v\":1,\"type\":\"user\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.000Z\",\"parts\":[{part}]}}\n"
        )
    };
    let digest = "0123456789abcdef".repeat(4);
    let line = user_line(&format!(
        r#"{{"type":"blob","mime":"application/pdf","blob":"{digest}","bytes":9}}"#
    ));
    let decoded = decode(line.as_bytes())?;
    let Record::User(Entry {
        kind: EntryKind::User { parts },
        ..
    }) = &decoded.record
    else {
        return Err("expected a user record".into());
    };
    assert_eq!(
        parts.as_slice(),
        &[JournalPart::Blob {
            mime: "application/pdf".into(),
            blob: digest.as_str().into(),
            bytes: 9,
        }]
    );
    assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());

    for part in [
        r#"{"type":"blob","blob":"ab12","bytes":9}"#,
        r#"{"type":"blob","mime":"application/pdf","bytes":9}"#,
        r#"{"type":"blob","mime":"application/pdf","blob":"ab12"}"#,
        r#"{"type":"blob","mime":"a/b","mime":"a/c","blob":"ab12","bytes":9}"#,
        r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"text":"x"}"#,
        r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"base64":"eA=="}"#,
        r#"{"type":"blob","mime":"a/b","blob":"ab12","bytes":9,"future":1}"#,
    ] {
        let line = user_line(part);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    Ok(())
}

#[test]
fn new_stop_literals_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let turn_end = |stop: &str| {
        format!(
            "{{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:20:00.000Z\",\"turn\":2,\"stop\":\"{stop}\",\"usage\":null,\"changes\":[]}}\n"
        )
    };
    let turn_cases = [
        ("length", TurnEndStop::Length),
        ("filter", TurnEndStop::Filter),
        ("max_steps", TurnEndStop::MaxSteps),
        ("aborted", TurnEndStop::Aborted),
    ];
    for (literal, expected) in turn_cases {
        let line = turn_end(literal);
        let decoded = decode(line.as_bytes())?;
        let Record::TurnEnd { stop, .. } = &decoded.record else {
            return Err("expected a turn_end record".into());
        };
        assert_eq!(*stop, expected);
        assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
    }
    assert!(matches!(
        decode(turn_end("tool_use").as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));

    let line = "{\"v\":1,\"type\":\"assistant\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"m\",\"content\":[],\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"stop\":\"filter\"}\n";
    let decoded = decode(line.as_bytes())?;
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant { stop, .. },
        ..
    }) = &decoded.record
    else {
        return Err("expected an assistant record".into());
    };
    assert_eq!(*stop, AssistantStop::Filter);
    assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
    assert!(matches!(
        decode(line.replace("\"filter\"", "\"max_steps\"").as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    Ok(())
}

#[test]
fn fork_rejects_non_user_anchor_as_not_user_entry() -> Result<(), Box<dyn std::error::Error>> {
    let header = Header {
        id: SessionId::new_v7(),
        at: "2026-09-25T10:15:30.000Z".parse()?,
        workspace: harness_workspace()?,
        product: Product::Dal,
        from: None,
    };
    let entry = Entry {
        id: EntryId::new(nz(7)),
        parent: None,
        at: "2026-09-25T10:15:30.000Z".parse()?,
        kind: EntryKind::Reminder {
            source: "rule:test".into(),
            text: "Reminder".into(),
        },
    };
    assert!(matches!(
        branch(
            &[Record::Reminder(entry)],
            None,
            BranchMode::Fork {
                at: EntryId::new(nz(7))
            },
            &header
        ),
        Err(BranchError::NotUserEntry { entry }) if entry.get() == 7
    ));
    Ok(())
}

#[test]
fn invalid_cost_is_rejected_not_nulled() -> Result<(), Box<dyn std::error::Error>> {
    let record = Record::TurnEnd {
        at: "2026-09-25T10:16:02.000Z".parse()?,
        turn: TurnId::new(nz(1)),
        stop: TurnEndStop::Done,
        usage: Some(Usage {
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: Some(f64::NAN),
        }),
        changes: Vec::new(),
    };
    assert!(matches!(encode(&record), Err(EncodeError::InvalidCost)));
    Ok(())
}

#[test]
fn scan_head_reads_only_tree_prefixes() {
    let user = b"{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":3,\"at\":\"t\",\"parts\":[]}\n";
    let head = scan_head(user);
    assert_eq!(
        head.map(|head| (head.id.get(), head.parent.map(EntryId::get), head.kind)),
        Some((4, Some(3), TreeKind::User))
    );
    assert_eq!(scan_head(b"{\"v\":1,\"type\":\"leaf\",\"to\":1}"), None);
    assert_eq!(scan_head(b"{\"v\":1,\"type\":\"user\"}"), None);
}

#[test]
fn tree_kind_tags_and_display_round_trip() {
    for (kind, tag) in [
        (TreeKind::User, "user"),
        (TreeKind::Assistant, "assistant"),
        (TreeKind::ToolResult, "tool_result"),
        (TreeKind::Reminder, "reminder"),
        (TreeKind::Model, "model"),
        (TreeKind::Thinking, "thinking"),
        (TreeKind::Approval, "approval"),
        (TreeKind::Mode, "mode"),
        (TreeKind::Compaction, "compaction"),
        (TreeKind::BranchSummary, "branch_summary"),
    ] {
        assert_eq!(kind.tag(), tag, "tag literal for {kind:?}");
        assert_eq!(kind.to_string(), tag, "Display for {kind:?}");
    }
}

#[test]
fn scan_head_rejects_corrupt_prefixes() {
    let shortest = b"{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":3,\"x\":0}";
    assert_eq!(
        scan_head(shortest).map(|head| (head.id.get(), head.parent.map(EntryId::get))),
        Some((4, Some(3))),
        "a minimal prefix line still scans"
    );
    let wrong_version = b"{\"v\":2,\"type\":\"user\",\"id\":7,\"parent\":null,\"x\":0}";
    assert_eq!(
        scan_head(wrong_version),
        None,
        "a non-v1 line must not scan as a v1 head"
    );
    assert_eq!(
        scan_head(b"{\"v\":1,\"type\":\"user"),
        None,
        "an unterminated tag must not scan or panic"
    );
    assert_eq!(
        scan_head(b"{\"v\":1,\"type\":\"user\",\"id\":,\"parent\":3,\"x\":0}"),
        None,
        "an id with no digits must not scan"
    );
    assert_eq!(
        scan_head(b"{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":,\"x\":0}"),
        None,
        "a parent with no digits must not scan"
    );
    assert_eq!(
        scan_head(b"{\"v\":1,\"type\":\"user\",\"id\":4,\"parent\":0,\"x\":0}"),
        None,
        "parent 0 is not a valid entry id"
    );
}

#[test]
fn decode_preserves_exit_codes_and_reasoning_tokens() {
    let ended = decode(
        b"{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:00.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"end\",\"outcome\":{\"exited\":137}}\n",
    )
    .expect("a nonzero exit code decodes");
    assert!(
        matches!(
            ended.record,
            Record::Job {
                event: JobEvent::Settled {
                    outcome: Some(JobOutcome::Exited { code: 137 })
                },
                ..
            }
        ),
        "the exit code survives the wire"
    );
    let signaled = decode(
        b"{\"v\":1,\"type\":\"job\",\"at\":\"2026-09-25T10:16:00.000Z\",\"job\":\"01927f40-0000-7000-8000-000000000002\",\"event\":\"end\",\"outcome\":{\"exited\":-9}}\n",
    )
    .expect("a negative exit code decodes");
    assert!(
        matches!(
            signaled.record,
            Record::Job {
                event: JobEvent::Settled {
                    outcome: Some(JobOutcome::Exited { code: -9 })
                },
                ..
            }
        ),
        "a signal-killed code stays negative"
    );
    let reasoned = decode(
        b"{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":5,\"cost_micro_usd\":0},\"changes\":[]}\n",
    )
    .expect("a non-null reasoning count decodes");
    assert!(
        matches!(
            reasoned.record,
            Record::TurnEnd {
                usage: Some(Usage {
                    reasoning_tokens: Some(5),
                    ..
                }),
                ..
            }
        ),
        "the reasoning count survives the wire"
    );
}

#[test]
fn branch_copies_path_and_labels() -> Result<(), Box<dyn std::error::Error>> {
    let header = Header {
        id: SessionId::new_v7(),
        at: "2026-09-25T10:15:30.000Z".parse()?,
        workspace: harness_workspace()?,
        product: Product::Dal,
        from: None,
    };
    let records = vec![
        Record::User(user_entry(1, None, "first")),
        Record::User(user_entry(2, Some(1), "second")),
        Record::User(user_entry(3, Some(2), "third")),
        Record::Label {
            at: "2026-09-25T10:15:31.000Z".parse()?,
            entry: EntryId::new(nz(1)),
            label: Some("kept".into()),
        },
        Record::Label {
            at: "2026-09-25T10:15:32.000Z".parse()?,
            entry: EntryId::new(nz(3)),
            label: Some("dropped".into()),
        },
    ];
    let forked = branch(
        &records,
        Some(EntryId::new(nz(3))),
        BranchMode::Fork {
            at: EntryId::new(nz(2)),
        },
        &header,
    )?;
    let ids: Vec<u64> = forked
        .records
        .iter()
        .filter_map(|record| record.entry().map(|entry| entry.id.get()))
        .collect();
    assert_eq!(ids, vec![1]);
    assert_eq!(
        forked.records.len(),
        2,
        "the path entry and its label copy; the off-path label does not"
    );
    assert_eq!(
        forked.anchor_parts,
        vec![JournalPart::Text {
            text: "second".into()
        }]
    );
    let source = forked.header.from.ok_or("fork sets from")?;
    assert_eq!(source.session, header.id);
    assert_eq!(source.entry.map(EntryId::get), Some(2));
    let cloned = branch(
        &records,
        Some(EntryId::new(nz(3))),
        BranchMode::Clone,
        &header,
    )?;
    let ids: Vec<u64> = cloned
        .records
        .iter()
        .filter_map(|record| record.entry().map(|entry| entry.id.get()))
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert_eq!(cloned.anchor_parts, []);
    assert!(matches!(
        branch(&[], None, BranchMode::Clone, &header),
        Err(BranchError::NoEntries)
    ));
    assert!(matches!(
        branch(
            &records,
            Some(EntryId::new(nz(3))),
            BranchMode::Fork {
                at: EntryId::new(nz(9)),
            },
            &header,
        ),
        Err(BranchError::UnknownEntry { .. })
    ));
    Ok(())
}

#[test]
fn text_part_with_escapes_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    // Borrowed-string decode cannot unescape; the member must round-trip
    // through an owned String or a journaled multi-line text fails on resume.
    let entry = user_entry(9, None, "first line\nquoted \"word\" and a \\backslash");
    let encoded = encode(&Record::User(entry))?;
    let decoded = decode(encoded.as_slice())?;
    let Record::User(decoded_entry) = &decoded.record else {
        panic!("decoded record is not user")
    };
    let EntryKind::User { parts } = &decoded_entry.kind else {
        panic!("decoded kind is not user")
    };
    let [JournalPart::Text { text }] = parts.as_slice() else {
        panic!("decoded one text part")
    };
    assert_eq!(&**text, "first line\nquoted \"word\" and a \\backslash");
    Ok(())
}

const OLD_TOOL_RESULT: &str = "{\"v\":1,\"type\":\"tool_result\",\"id\":6,\"parent\":5,\"at\":\"2026-09-25T10:15:35.431Z\",\"call\":\"toolu_01\",\"name\":\"read\",\"error\":false,\"parts\":[{\"type\":\"text\",\"text\":\"ok\"}],\"changes\":[]";

fn tool_result_elapsed(line: &str) -> Result<Option<u64>, DecodeError> {
    match decode(line.as_bytes())?.record {
        Record::ToolResult(Entry {
            kind: EntryKind::ToolResult { elapsed_ms, .. },
            ..
        }) => Ok(elapsed_ms),
        other => panic!("decoded record is not a tool result: {other:?}"),
    }
}

#[test]
fn tool_result_elapsed_round_trips_and_old_lines_decode_without_it()
-> Result<(), Box<dyn std::error::Error>> {
    let old = format!("{OLD_TOOL_RESULT}}}\n");
    assert_eq!(tool_result_elapsed(&old)?, None);
    assert_eq!(
        String::from_utf8(encode(&decode(old.as_bytes())?.record)?)?,
        old
    );

    let timed = format!("{OLD_TOOL_RESULT},\"elapsed_ms\":1234}}\n");
    assert_eq!(tool_result_elapsed(&timed)?, Some(1234));
    assert_eq!(
        String::from_utf8(encode(&decode(timed.as_bytes())?.record)?)?,
        timed
    );

    let null = format!("{OLD_TOOL_RESULT},\"elapsed_ms\":null}}\n");
    assert_eq!(tool_result_elapsed(&null)?, None);

    let bad = format!("{OLD_TOOL_RESULT},\"elapsed_ms\":\"slow\"}}\n");
    assert!(matches!(
        decode(bad.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    Ok(())
}

#[test]
fn tool_result_kind_json_carries_elapsed_only_when_measured()
-> Result<(), Box<dyn std::error::Error>> {
    let kind = |elapsed_ms| EntryKind::ToolResult {
        call: CallId::new("call_1"),
        name: "read".into(),
        error: false,
        parts: Vec::new(),
        changes: Vec::new(),
        elapsed_ms,
    };
    let timed = sonic_rs::to_string(&kind(Some(7)))?;
    assert!(timed.contains("\"elapsed_ms\":7"), "{timed}");
    assert_eq!(sonic_rs::from_str::<EntryKind>(&timed)?, kind(Some(7)));

    let untimed = sonic_rs::to_string(&kind(None))?;
    assert!(!untimed.contains("elapsed_ms"), "{untimed}");
    assert_eq!(sonic_rs::from_str::<EntryKind>(&untimed)?, kind(None));
    Ok(())
}
