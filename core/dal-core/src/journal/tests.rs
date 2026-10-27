use super::*;
use crate::update::ResolutionCause;

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

fn assistant_entry(model: &str, route: Option<AssistantRoute>) -> Entry {
    Entry {
        id: EntryId::new(nz(5)),
        parent: None,
        at: "2026-09-25T10:15:35.410Z"
            .parse()
            .expect("valid fixture timestamp"),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: model.into(),
            provider: None,
            content: Vec::new(),
            usage: Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                premium_requests: None,
            },
            stop: AssistantStop::Done,
            route,
            replay: None,
        },
    }
}

fn assistant_line(model: &str, route: Option<&str>) -> String {
    let route_member = route.map_or(String::new(), |route| format!(r#","route":{route}"#));
    format!(
        "{{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":null,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"{model}\",\"content\":[],\"usage\":{{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}},\"stop\":\"done\"{route_member}}}\n"
    )
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

const RESOLVED_HEAD: &str = "{\"v\":1,\"type\":\"resolved\",\"at\":\"2026-09-25T10:15:36.000Z\",\"request\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"answer\":\"cancel\",\"by\":\"core\"";

/// Every resolution cause beside its durable literal.
const RESOLUTION_CAUSES: [(ResolutionCause, &str); 5] = [
    (ResolutionCause::Answered, "answered"),
    (ResolutionCause::TimedOut, "timed_out"),
    (ResolutionCause::Cancelled, "cancelled"),
    (ResolutionCause::Withdrawn, "withdrawn"),
    (ResolutionCause::NoController, "no_controller"),
];

fn resolved_line(tail: &str) -> String {
    format!("{RESOLVED_HEAD}{tail}}}\n")
}

fn resolved_record(
    was_default: bool,
    cause: Option<ResolutionCause>,
) -> Result<Record, Box<dyn std::error::Error>> {
    Ok(Record::Resolved {
        at: "2026-09-25T10:15:36.000Z".parse()?,
        request: RequestId::parse("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f")?,
        answer: Answer::Cancel,
        by: ClientId::new("core"),
        was_default,
        cause,
    })
}

#[test]
fn resolved_cause_round_trips_every_literal_through_the_codec()
-> Result<(), Box<dyn std::error::Error>> {
    for (cause, literal) in RESOLUTION_CAUSES {
        for (was_default, default_member) in [(false, ""), (true, ",\"was_default\":true")] {
            let line = resolved_line(&format!("{default_member},\"cause\":\"{literal}\""));
            let record = resolved_record(was_default, Some(cause))?;
            assert_eq!(decode(line.as_bytes())?.record, record, "{line}");
            assert_eq!(encode(&record)?.as_slice(), line.as_bytes(), "{line}");
        }
    }

    // Decoding ignores member order; encoding always writes the declared
    // order, with `cause` after `was_default`.
    let reordered = resolved_line(",\"cause\":\"timed_out\",\"was_default\":true");
    let canonical = resolved_line(",\"was_default\":true,\"cause\":\"timed_out\"");
    let record = resolved_record(true, Some(ResolutionCause::TimedOut))?;
    assert_eq!(decode(reordered.as_bytes())?.record, record);
    assert_eq!(encode(&record)?.as_slice(), canonical.as_bytes());
    Ok(())
}

#[test]
fn legacy_resolved_records_have_no_cause_and_re_encode_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    for (tail, was_default) in [("", false), (",\"was_default\":true", true)] {
        let line = resolved_line(tail);
        let legacy = resolved_record(was_default, None)?;
        assert_eq!(decode(line.as_bytes())?.record, legacy, "{line}");
        assert_eq!(encode(&legacy)?.as_slice(), line.as_bytes(), "{line}");
    }
    Ok(())
}

#[test]
fn resolved_cause_rejects_unknown_literals_and_non_string_members() {
    // A present member never reads as a missing cause: the closed payload
    // refuses a corrupt or future spelling instead of fabricating `None`.
    let cases = [
        "\"soon\"",
        "\"\"",
        "\"Answered\"",
        "\"timed-out\"",
        "\"answered \"",
        "null",
        "7",
        "true",
        "[\"answered\"]",
        "{\"answered\":true}",
    ];
    for bad in cases {
        let line = resolved_line(&format!(",\"cause\":{bad}"));
        let marker = "\"cause\":";
        let expected_offset = line.find(marker).unwrap() + marker.len();
        assert!(
            matches!(
                decode(line.as_bytes()),
                Err(DecodeError::Invalid { offset, .. }) if offset == expected_offset
            ),
            "{bad}"
        );
    }
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
        decode(b"{\"v\":3,\"type\":\"leaf\",\"at\":\"2026-09-25T14:05:00.000Z\",\"to\":41}"),
        Err(DecodeError::UnsupportedVersion { found: 3 })
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
        "{\"v\":3,\"type\":\"leaf\",\"type\":\"other\",\"at\":\"x\",\"to\":1}",
        "{\"type\":\"leaf\",\"to\":1,\"to\":2,\"v\":3}",
        "{\"to\":1,\"v\":3,\"type\":\"leaf\",\"to\":2}",
    ] {
        assert!(matches!(
            decode(line.as_bytes()),
            Err(DecodeError::UnsupportedVersion { found: 3 })
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
            premium_requests: None,
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
                provider: None,
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
fn assistant_route_round_trips_every_kind_through_both_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            AssistantRoute::Api(Family::Chat),
            "probe/model",
            r#""route":{"api":"openai_chat"}"#,
        ),
        (
            AssistantRoute::Synthetic,
            "probe/model",
            r#""route":"synthetic""#,
        ),
        (
            AssistantRoute::Harness,
            "dalgon/normal",
            r#""route":"harness""#,
        ),
    ];
    for (route, model, member) in cases {
        let kind = assistant_entry(model, Some(route)).kind;
        let serde_text = sonic_rs::to_string(&kind)?;
        assert!(serde_text.contains(member), "{serde_text}");
        assert_eq!(sonic_rs::from_str::<EntryKind>(&serde_text)?, kind);

        let record = Record::Assistant(assistant_entry(model, Some(route)));
        let encoded = encode(&record)?;
        let line = std::str::from_utf8(&encoded)?;
        assert!(line.contains(member), "{line}");
        assert_eq!(decode(&encoded)?.record, record);
    }
    Ok(())
}

#[test]
fn assistant_route_accepts_escaped_json_names() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        (
            r#"{"a\u0070i":"openai_chat"}"#,
            AssistantRoute::Api(Family::Chat),
        ),
        (
            r#"{"api":"openai_\u0063hat"}"#,
            AssistantRoute::Api(Family::Chat),
        ),
        (r#""syn\u0074hetic""#, AssistantRoute::Synthetic),
        (r#""har\u006eess""#, AssistantRoute::Harness),
    ];
    let serde_base = sonic_rs::to_string(&assistant_entry("dalgon/normal", None).kind)?;
    for (value, route) in cases {
        let expected = assistant_entry("dalgon/normal", Some(route));
        let line = assistant_line("dalgon/normal", Some(value));
        assert_eq!(
            decode(line.as_bytes())?.record,
            Record::Assistant(expected.clone())
        );
        let injected = format!(
            "{},\"route\":{value}}}",
            &serde_base[..serde_base.len() - 1]
        );
        assert_eq!(sonic_rs::from_str::<EntryKind>(&injected)?, expected.kind);
    }
    Ok(())
}

#[test]
fn assistant_route_keeps_colliding_identifiers_distinct() -> Result<(), Box<dyn std::error::Error>>
{
    // `probe/model` parses both as a Chat-family API model id and as a
    // synthetic id; the route member is what keeps the two records
    // distinguishable on identical identifiers.
    let api = Record::Assistant(assistant_entry(
        "probe/model",
        Some(AssistantRoute::Api(Family::Chat)),
    ));
    let synthetic = Record::Assistant(assistant_entry(
        "probe/model",
        Some(AssistantRoute::Synthetic),
    ));
    let api_line = encode(&api)?;
    let synthetic_line = encode(&synthetic)?;
    assert_ne!(api_line, synthetic_line);
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant {
            route: api_route, ..
        },
        ..
    }) = decode(&api_line)?.record
    else {
        return Err("expected an assistant record".into());
    };
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant {
            route: synthetic_route,
            ..
        },
        ..
    }) = decode(&synthetic_line)?.record
    else {
        return Err("expected an assistant record".into());
    };
    assert_eq!(api_route, Some(AssistantRoute::Api(Family::Chat)));
    assert_eq!(synthetic_route, Some(AssistantRoute::Synthetic));
    Ok(())
}

#[test]
fn absent_assistant_route_is_unknown_and_stays_omitted() -> Result<(), Box<dyn std::error::Error>> {
    let kind = assistant_entry("m", None).kind;
    let serde_text = sonic_rs::to_string(&kind)?;
    assert!(!serde_text.contains("\"route\""), "{serde_text}");
    assert_eq!(sonic_rs::from_str::<EntryKind>(&serde_text)?, kind);

    // A record from before the member existed decodes with unknown
    // provenance and re-encodes byte for byte.
    let line = assistant_line("m", None);
    let decoded = decode(line.as_bytes())?;
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant { route, .. },
        ..
    }) = &decoded.record
    else {
        return Err("expected an assistant record".into());
    };
    assert_eq!(*route, None);
    assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
    Ok(())
}

#[test]
fn malformed_assistant_route_is_rejected_on_both_paths() -> Result<(), Box<dyn std::error::Error>> {
    let serde_base = sonic_rs::to_string(&assistant_entry("probe/model", None).kind)?;
    let malformed = [
        // A present null is malformed, not unknown.
        "null",
        // An invalid discriminator, including a bare family name and the
        // bare literal `api`.
        "\"chat\"",
        "\"api\"",
        "12",
        "true",
        "[]",
        // A family wire misspelling inside the route object.
        "{\"api\":\"chat\"}",
        // Unit variants accept no payload, not even null.
        "{\"synthetic\":{\"id\":\"probe/model\"}}",
        "{\"synthetic\":null}",
        "{\"harness\":{\"id\":\"dalgon/normal\"}}",
        // No tag, or a second member next to `api`.
        "{}",
        "{\"api\":\"openai_chat\",\"harness\":null}",
    ];
    for route in malformed {
        let line = assistant_line("probe/model", Some(route));
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
        let injected = format!(
            "{},\"route\":{route}}}",
            &serde_base[..serde_base.len() - 1]
        );
        assert!(
            sonic_rs::from_str::<EntryKind>(&injected).is_err(),
            "{injected}"
        );
    }
    // A duplicate record member is rejected before its value is read.
    let duplicated = assistant_line("probe/model", Some("\"synthetic\",\"route\":\"synthetic\""));
    assert!(matches!(
        decode(duplicated.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    Ok(())
}

#[test]
fn assistant_route_must_describe_the_model_id() -> Result<(), Box<dyn std::error::Error>> {
    // A synthetic provenance needs a synthetic id; a harness provenance
    // needs a harness id; both spellings are refused on every path.
    let synthetic_kind = assistant_entry("no-slash", Some(AssistantRoute::Synthetic)).kind;
    let serde_text = sonic_rs::to_string(&synthetic_kind)?;
    assert!(
        sonic_rs::from_str::<EntryKind>(&serde_text).is_err(),
        "{serde_text}"
    );
    let harness_kind = assistant_entry("probe/model", Some(AssistantRoute::Harness)).kind;
    let serde_text = sonic_rs::to_string(&harness_kind)?;
    assert!(
        sonic_rs::from_str::<EntryKind>(&serde_text).is_err(),
        "{serde_text}"
    );

    assert!(matches!(
        decode(assistant_line("Upper/x", Some("\"synthetic\"")).as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    assert!(matches!(
        decode(assistant_line("dalgon/fast", Some("\"harness\"")).as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    // The API route keeps the entry's non-empty rule and adds no grammar
    // of its own: an id that also parses as a synthetic name stays valid.
    let colliding = Record::Assistant(assistant_entry(
        "probe/model",
        Some(AssistantRoute::Api(Family::Chat)),
    ));
    assert_eq!(decode(&encode(&colliding)?)?.record, colliding);
    Ok(())
}

#[test]
fn encode_rejects_assistant_route_the_decoder_would_reject() {
    let record =
        |model: &str, route: AssistantRoute| Record::Assistant(assistant_entry(model, Some(route)));
    assert!(matches!(
        encode(&record("Upper/x", AssistantRoute::Synthetic)),
        Err(EncodeError::InvalidRoute(
            RouteError::InvalidSyntheticId { .. }
        ))
    ));
    assert!(matches!(
        encode(&record("dalgon/fast", AssistantRoute::Harness)),
        Err(EncodeError::InvalidRoute(
            RouteError::InvalidHarnessId { .. }
        ))
    ));
    assert!(encode(&record("probe/model", AssistantRoute::Synthetic)).is_ok());
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
                provider: None,
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
            premium_requests: None,
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
fn tree_kind_tags_match_scan_head_for_every_kind() {
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
        let line = format!("{{\"v\":1,\"type\":\"{tag}\",\"id\":4,\"parent\":3,\"x\":0}}");
        assert_eq!(
            scan_head(line.as_bytes()).map(|head| (
                head.id.get(),
                head.parent.map(EntryId::get),
                head.kind
            )),
            Some((4, Some(3), kind)),
            "scan_head labels a {tag} prefix line"
        );
    }
    assert_eq!(
        scan_head(b"{\"v\":1,\"type\":\"bogus\",\"id\":4,\"parent\":3,\"x\":0}"),
        None,
        "an unknown tag does not scan"
    );
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

/// An empty object has a tail too: `{}` is a complete record missing `v`,
/// not trailing garbage. Reverting the derived tail reports
/// `Invalid { trailing bytes }` instead of `MissingVersion`.
#[test]
fn decode_an_empty_object_is_missing_version_not_trailing_bytes() {
    assert!(matches!(decode(b"{}"), Err(DecodeError::MissingVersion)));
    // A truncated line still fails as malformed.
    assert!(matches!(decode(b"{"), Err(DecodeError::Invalid { .. })));
    assert!(matches!(
        decode(b"{\"v\":3,\"type\":42,\"garbage\":[}"),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn legacy_record_rejects_the_reservation_only_version() {
    let legacy = br#"{"v":1,"type":"turn_start","at":"2026-10-13T00:00:00.000Z","turn":1}"#;
    assert!(
        matches!(decode(legacy), Ok(decoded) if matches!(decoded.record, Record::TurnStart { .. }))
    );
    let wrong_version = br#"{"v":2,"type":"turn_start","at":"2026-10-13T00:00:00.000Z","turn":1}"#;
    assert!(matches!(
        decode(wrong_version),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn reservation_format_preserves_legacy_bytes_and_rejects_old_version()
-> Result<(), Box<dyn std::error::Error>> {
    let reserved = br#"{"v":2,"type":"turn_reserved","at":"2026-10-13T00:00:00.000Z","turn":2}"#;
    let decoded = decode(reserved)?;
    assert!(matches!(decoded.record, Record::TurnReserved { turn, .. } if turn.get() == 2));
    let mut expected = reserved.to_vec();
    expected.push(b'\n');
    assert_eq!(encode(&decoded.record)?, expected);
    assert!(scan_head(reserved).is_none());
    let old = br#"{"v":1,"type":"turn_reserved","at":"2026-10-13T00:00:00.000Z","turn":2}"#;
    assert!(matches!(decode(old), Err(DecodeError::Invalid { .. })));
    let future = br#"{"v":3,"type":"turn_reserved","at":"2026-10-13T00:00:00.000Z","turn":2}"#;
    assert!(matches!(
        decode(future),
        Err(DecodeError::UnsupportedVersion { found: 3 })
    ));
    Ok(())
}

fn assistant_with_replay(content: Vec<Block>, replay: Option<RawJson>) -> Record {
    Record::Assistant(Entry {
        id: EntryId::new(nz(5)),
        parent: None,
        at: "2026-09-25T10:15:35.410Z"
            .parse()
            .expect("valid fixture timestamp"),
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "m".into(),
            provider: None,
            content,
            usage: Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                premium_requests: None,
            },
            stop: AssistantStop::Done,
            route: None,
            replay,
        },
    })
}

/// Legacy assistant bytes predate block and entry replay: a text
/// `"replay":null` reads as absent, a tool call with no replay member stays
/// absent, and the entry carries no replay. The line re-encodes byte for byte.
#[test]
fn legacy_assistant_blocks_without_replay_re_encode_identically()
-> Result<(), Box<dyn std::error::Error>> {
    let line = "{\"v\":1,\"type\":\"assistant\",\"id\":5,\"parent\":4,\"at\":\"2026-09-25T10:15:35.410Z\",\"api\":\"anthropic\",\"model\":\"claude-opus-5\",\"content\":[{\"type\":\"text\",\"text\":\"I will read the lexer.\",\"replay\":null},{\"type\":\"tool_call\",\"id\":\"toolu_01\",\"name\":\"read\",\"input\":{\"path\":\"lib/lexer.ml\"}}],\"usage\":{\"input\":1200,\"output\":85,\"cache_read\":0,\"cache_write\":1100,\"reasoning\":null,\"cost_micro_usd\":4210},\"stop\":\"tool_use\"}\n";
    let decoded = decode(line.as_bytes())?;
    let Record::Assistant(entry) = &decoded.record else {
        return Err("expected an assistant record".into());
    };
    let EntryKind::Assistant {
        content, replay, ..
    } = &entry.kind
    else {
        return Err("expected an assistant entry".into());
    };
    assert_eq!(*replay, None);
    assert!(matches!(
        content.as_slice(),
        [Block::Text { text, replay: None }, Block::ToolCall { replay: None, .. }]
        if text.as_ref() == "I will read the lexer."
    ));
    assert_eq!(encode(&decoded.record)?.as_slice(), line.as_bytes());
    Ok(())
}

/// A present replay payload is opaque bytes: interior whitespace, escapes and
/// non-ASCII survive the round trip on text blocks, tool calls and the
/// assistant entry alike, and decode hands back the same value.
#[test]
fn present_replay_payloads_survive_byte_for_byte() -> Result<(), Box<dyn std::error::Error>> {
    let raw = "{\"b\":2,  \"a\":\"h\u{00e9}llo \\\"w\u{00f6}rld\\\"\\n \u{2603} \u{00e9}\"}";
    let replay = || RawJson::parse(raw);
    let record = Record::Assistant(Entry {
        id: EntryId::new(nz(5)),
        parent: None,
        at: "2026-09-25T10:15:35.410Z".parse()?,
        kind: EntryKind::Assistant {
            api: Family::Anthropic,
            model: "m".into(),
            provider: None,
            content: vec![
                Block::Text {
                    text: "hi".into(),
                    replay: Some(replay()?),
                },
                Block::ToolCall {
                    id: CallId::new("toolu_01"),
                    name: "read".into(),
                    input: RawJson::parse("{}")?,
                    replay: Some(replay()?),
                },
            ],
            usage: Usage {
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                premium_requests: None,
            },
            stop: AssistantStop::Done,
            route: None,
            replay: Some(replay()?),
        },
    });
    let line = String::from_utf8(encode(&record)?)?;
    assert_eq!(line.matches(raw).count(), 3, "replay bytes changed: {line}");
    assert_eq!(decode(line.as_bytes())?.record, record);
    assert_eq!(
        encode(&decode(line.as_bytes())?.record)?,
        line.as_bytes(),
        "a replay-bearing line re-encodes identically"
    );
    Ok(())
}

/// An explicit entry-level replay null is malformed, not absent: like `route`,
/// a present member never reads as a missing one.
#[test]
fn assistant_entry_rejects_an_explicit_replay_null() {
    let line = assistant_line("m", None)
        .replace("\"stop\":\"done\"}", "\"stop\":\"done\",\"replay\":null}");
    assert!(
        matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
        "{line}"
    );
}

/// The direct `EntryKind` serde path keeps entry replay: a present payload
/// round-trips through the derived spelling and the wire line alike, instead
/// of dropping silently while the journal decoder preserves it.
#[test]
fn assistant_replay_round_trips_through_direct_entry_decode()
-> Result<(), Box<dyn std::error::Error>> {
    let payload = RawJson::parse(r#"{"api":"anthropic"}"#)?;
    let record = assistant_with_replay(Vec::new(), Some(payload));
    let Record::Assistant(entry) = &record else {
        return Err("expected an assistant record".into());
    };
    let serde_text = sonic_rs::to_string(&entry.kind)?;
    assert!(
        serde_text.contains(r#""replay":{"api":"anthropic"}"#),
        "{serde_text}"
    );
    assert_eq!(sonic_rs::from_str::<EntryKind>(&serde_text)?, entry.kind);
    let line = String::from_utf8(encode(&record)?)?;
    assert!(line.contains(r#""replay":{"api":"anthropic"}"#), "{line}");
    assert_eq!(decode(line.as_bytes())?.record, record);
    Ok(())
}

/// A present `null` entry replay is malformed on the direct serde path too:
/// only an absent member reads as missing replay.
#[test]
fn direct_entry_decode_rejects_an_explicit_replay_null() -> Result<(), Box<dyn std::error::Error>> {
    let record = assistant_with_replay(Vec::new(), None);
    let Record::Assistant(entry) = &record else {
        return Err("expected an assistant record".into());
    };
    let serde_text = sonic_rs::to_string(&entry.kind)?;
    assert!(!serde_text.contains("\"replay\""), "{serde_text}");
    let injected = format!("{},\"replay\":null}}", &serde_text[..serde_text.len() - 1]);
    assert!(
        sonic_rs::from_str::<EntryKind>(&injected).is_err(),
        "{injected}"
    );
    Ok(())
}

/// The encoder refuses a caller-built null entry replay: the member is
/// skip-None, so `Some(null)` would write the explicit null the decoder
/// rejects. The value is reported, never normalized to absent.
#[test]
fn encode_rejects_null_assistant_replay_the_decoder_would_reject() {
    let record = assistant_with_replay(Vec::new(), Some(RawJson::null()));
    assert!(matches!(encode(&record), Err(EncodeError::NullReplay)));
}

/// A tool call without replay writes no replay member: the encoded line and
/// the serde spelling both stay free of it, and both paths round-trip.
#[test]
fn tool_call_without_replay_writes_no_replay_member() -> Result<(), Box<dyn std::error::Error>> {
    let record = assistant_with_replay(
        vec![Block::ToolCall {
            id: CallId::new("toolu_01"),
            name: "read".into(),
            input: RawJson::parse("{}")?,
            replay: None,
        }],
        None,
    );
    let line = String::from_utf8(encode(&record)?)?;
    assert!(!line.contains("replay"), "{line}");
    assert_eq!(decode(line.as_bytes())?.record, record);
    let serde_text = sonic_rs::to_string(&Block::ToolCall {
        id: CallId::new("toolu_01"),
        name: "read".into(),
        input: RawJson::parse("{}")?,
        replay: None,
    })?;
    assert!(!serde_text.contains("replay"), "{serde_text}");
    assert_eq!(
        sonic_rs::from_str::<Block>(&serde_text)?,
        Block::ToolCall {
            id: CallId::new("toolu_01"),
            name: "read".into(),
            input: RawJson::parse("{}")?,
            replay: None,
        }
    );
    Ok(())
}
/// The fold writes `Block::Reasoning { replay: RawJson::null() }` for reasoning
/// text a provider streamed without a replay event, and the encoder spells it
/// `"replay":null`. The reader accepts every line the writer emits: the
/// placeholder survives line decode, re-encodes byte for byte, and the entry
/// level replay stays absent.
#[test]
fn reasoning_block_with_null_placeholder_replay_round_trips()
-> Result<(), Box<dyn std::error::Error>> {
    let record = assistant_with_replay(
        vec![Block::Reasoning {
            text: "weighing the options".into(),
            replay: RawJson::null(),
        }],
        None,
    );
    let line = String::from_utf8(encode(&record)?)?;
    assert!(
        line.contains(r#"{"type":"reasoning","text":"weighing the options","replay":null}"#),
        "{line}"
    );
    let decoded = decode(line.as_bytes())?;
    assert_eq!(decoded.record, record);
    assert_eq!(encode(&decoded.record)?, line.as_bytes());
    Ok(())
}

/// A reasoning block keeps requiring its `replay` member: only an explicit
/// `null` reads as the placeholder, an absent member is still malformed.
/// Block-level `null` on text and tool calls still collapses to absent, and
/// the entry-level `null` stays an error beside a null reasoning block.
#[test]
fn reasoning_replay_null_is_accepted_but_absent_is_not() -> Result<(), Box<dyn std::error::Error>> {
    let with_content = |content: &str| {
        assistant_line("m", None).replace("\"content\":[]", &format!("\"content\":[{content}]"))
    };

    let null = with_content(r#"{"type":"reasoning","text":"t","replay":null}"#);
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant {
            content, replay, ..
        },
        ..
    }) = decode(null.as_bytes())?.record
    else {
        return Err("expected an assistant record".into());
    };
    assert_eq!(replay, None);
    assert_eq!(
        content,
        vec![Block::Reasoning {
            text: "t".into(),
            replay: RawJson::null(),
        }]
    );

    let spaced = with_content(r#"{"type":"reasoning","text":"t","replay": null }"#);
    assert!(decode(spaced.as_bytes()).is_ok(), "{spaced}");

    let absent = with_content(r#"{"type":"reasoning","text":"t"}"#);
    assert!(
        matches!(decode(absent.as_bytes()), Err(DecodeError::Invalid { .. })),
        "{absent}"
    );

    let entry_null = null.replace("\"stop\":\"done\"}", "\"stop\":\"done\",\"replay\":null}");
    assert!(
        matches!(
            decode(entry_null.as_bytes()),
            Err(DecodeError::Invalid { .. })
        ),
        "{entry_null}"
    );

    let collapsed = with_content(
        r#"{"type":"text","text":"a","replay":null},{"type":"tool_call","id":"c","name":"read","input":{},"replay":null}"#,
    );
    let Record::Assistant(Entry {
        kind: EntryKind::Assistant { content, .. },
        ..
    }) = decode(collapsed.as_bytes())?.record
    else {
        return Err("expected an assistant record".into());
    };
    assert!(matches!(
        content.as_slice(),
        [
            Block::Text { replay: None, .. },
            Block::ToolCall { replay: None, .. }
        ]
    ));
    Ok(())
}

/// The direct `Block` serde path is the twin of the line decoder: a reasoning
/// block's `replay: null` is the placeholder, an absent `replay` is an error.
#[test]
fn direct_block_decode_accepts_reasoning_replay_null() -> Result<(), Box<dyn std::error::Error>> {
    let block = Block::Reasoning {
        text: "t".into(),
        replay: RawJson::null(),
    };
    let serde_text = sonic_rs::to_string(&block)?;
    assert_eq!(
        serde_text,
        r#"{"type":"reasoning","text":"t","replay":null}"#
    );
    assert_eq!(sonic_rs::from_str::<Block>(&serde_text)?, block);
    assert!(sonic_rs::from_str::<Block>(r#"{"type":"reasoning","text":"t"}"#).is_err());
    Ok(())
}

fn end_reasoning_turn(
    session: &mut crate::Session,
    turn: crate::TurnId,
    stamp: crate::Timestamp,
    effects: &mut Vec<crate::Effect>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::{Event, Family, Inference, ModelRoute, Stop, StreamChannel, StreamEvent, Usage};

    session.step(
        Event::StreamEnded {
            turn,
            model: ModelRoute::Api {
                family: Family::Chat,
                model: "test-model".into(),
                provider: None,
            },
            family: Family::Chat,
            result: Ok(Inference {
                events: vec![
                    StreamEvent::Delta {
                        channel: StreamChannel::Thinking,
                        text: "weighing ".into(),
                    },
                    StreamEvent::Delta {
                        channel: StreamChannel::Thinking,
                        text: "it".into(),
                    },
                    StreamEvent::Delta {
                        channel: StreamChannel::Text,
                        text: "done".into(),
                    },
                    StreamEvent::Usage(Usage {
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        output_tokens: 1,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        premium_requests: None,
                        cost_usd: None,
                    }),
                    StreamEvent::Stop(Stop::EndTurn),
                ],
            }),
            partial: None,
        },
        stamp,
        effects,
    )?;
    Ok(())
}

/// End to end inside the core: a turn whose provider streamed reasoning text
/// and no replay event journals the null placeholder, and the lines the fold
/// emitted re-open as a session. Every record the writer produced must read
/// back through the journal decoder.
#[test]
fn folded_reasoning_turn_without_replay_event_re_opens() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        ClientId, Command, Effect, Event, Expect, HookEvent, HookOutcome, HookVerdict, Part, Phase,
        Session,
    };

    let stamp = crate::Timestamp::UNIX_EPOCH;
    let mut session = Session::replay([], stamp)?.0;
    let mut effects = Vec::new();
    session.step(
        Event::Command {
            cmd: Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text {
                    text: "question".into(),
                }],
            },
            by: ClientId::new("test"),
        },
        stamp,
        &mut effects,
    )?;
    let Phase::Opening { turn, .. } = session.phase() else {
        return Err("the prompt did not open a turn".into());
    };
    let turn = *turn;
    session.step(
        Event::Guard {
            turn,
            call: None,
            extension: None,
            outcome: HookOutcome::new(HookEvent::BeforeTurn, HookVerdict::BeforeTurn(None))?,
        },
        stamp,
        &mut effects,
    )?;
    end_reasoning_turn(&mut session, turn, stamp, &mut effects)?;

    let mut records = Vec::new();
    for effect in &effects {
        if let Effect::Emit(emit) = effect {
            records.extend(emit.records.iter().cloned());
        }
    }
    let content = records
        .iter()
        .find_map(|record| match record {
            Record::Assistant(Entry {
                kind: EntryKind::Assistant { content, .. },
                ..
            }) => Some(content.clone()),
            _ => None,
        })
        .ok_or("the stream journaled no assistant entry")?;
    assert!(
        matches!(
            content.as_slice(),
            [Block::Reasoning { text, replay }, Block::Text { .. }]
            if text.as_ref() == "weighing it" && replay.as_str() == "null"
        ),
        "{content:?}"
    );

    let lines = records.iter().map(encode).collect::<Result<Vec<_>, _>>()?;
    for (record, line) in records.iter().zip(&lines) {
        assert_eq!(&decode(line)?.record, record);
    }
    Session::replay_lines(lines.iter().map(Vec::as_slice), stamp)?;
    Ok(())
}

// Private audit records: v1 wire, rejection, parity spot-check, branch omission.
// Full old-journal byte parity lives in `canonical_vectors_round_trip_byte_for_byte`
// and `resolved_and_extended_records_round_trip`; the spot check below only proves
// the private codec did not perturb old lines. Wire order follows the landed
// codec: private_loop type/at/loop/who/turn?, private_start
// type/at/loop/token/who/turn?/round/call/name/args, private_end
// type/at/loop/token/outcome with settled {type/is_error/parts} and closed
// indeterminate, all v1, absent turn means None.
fn private_loop_id(value: &str) -> PrivateLoopId {
    PrivateLoopId::parse(value).expect("valid fixture loop id")
}

fn private_token_id(value: &str) -> PrivateEffectId {
    PrivateEffectId::parse(value).expect("valid fixture token id")
}

fn private_owner() -> Owner {
    Owner::Extension {
        name: "fusion".into(),
        origin: "bundled".into(),
    }
}

fn private_at() -> jiff::Timestamp {
    "2026-09-25T10:15:35.420Z"
        .parse()
        .expect("valid fixture timestamp")
}

fn private_settled_parts() -> Vec<JournalPart> {
    let digest = "0123456789abcdef".repeat(4);
    vec![
        JournalPart::Text {
            text: "checked 3 files".into(),
        },
        JournalPart::Blob {
            mime: "application/pdf".into(),
            blob: digest.as_str().into(),
            bytes: 9,
        },
    ]
}

fn private_round_trip_ids() -> (PrivateLoopId, PrivateEffectId, Owner) {
    (
        private_loop_id("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f"),
        private_token_id("01927f40-0000-7000-8000-000000000002"),
        private_owner(),
    )
}

#[test]
fn private_loop_and_start_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (loop_a, token_a, who) = private_round_trip_ids();

    let loop_rec = Record::PrivateLoop {
        at: private_at(),
        r#loop: loop_a,
        who: who.clone(),
        turn: None,
    };
    let encoded = encode(&loop_rec)?;
    let text = String::from_utf8(encoded.clone())?;
    assert!(
        text.starts_with(r#"{"v":1,"type":"private_loop""#),
        "{text}"
    );
    assert!(!text.contains("\"turn\""), "{text}");
    assert_eq!(loop_rec.tag(), "private_loop");
    assert!(loop_rec.entry().is_none());
    assert_eq!(decode(&encoded)?.record, loop_rec);
    assert_eq!(encode(&decode(&encoded)?.record)?, encoded);

    let args = RawJson::parse(r#"{"path":"lib/lexer.ml"}"#)?;
    let args_text = args.as_str().to_owned();
    let start = Record::PrivateStart {
        at: private_at(),
        r#loop: loop_a,
        token: token_a,
        who: who.clone(),
        turn: None,
        round: 2,
        call: CallId::new("toolu_01"),
        name: "read".into(),
        args,
    };
    let encoded = encode(&start)?;
    let text = String::from_utf8(encoded.clone())?;
    assert!(
        text.starts_with(r#"{"v":1,"type":"private_start""#),
        "{text}"
    );
    assert!(!text.contains("\"turn\""), "{text}");
    assert!(text.contains(r#""round":2"#), "{text}");
    assert!(text.contains(r#""call":"toolu_01""#), "{text}");
    assert_eq!(start.tag(), "private_start");
    assert!(start.entry().is_none());
    let decoded = decode(&encoded)?.record;
    assert_eq!(decoded, start);
    assert_eq!(encode(&decoded)?, encoded);
    let Record::PrivateStart {
        r#loop,
        token,
        turn,
        round,
        call,
        name,
        args,
        ..
    } = &decoded
    else {
        return Err("expected a private start".into());
    };
    assert_eq!(*r#loop, loop_a);
    assert_eq!(*token, token_a);
    assert_eq!(*turn, None);
    assert_eq!(*round, 2);
    assert_eq!(call.as_str(), "toolu_01");
    assert_eq!(name.as_ref(), "read");
    assert_eq!(args.as_str(), args_text.as_str());
    Ok(())
}

#[test]
fn private_records_round_trip_settled() -> Result<(), Box<dyn std::error::Error>> {
    let (loop_a, token_a, _) = private_round_trip_ids();

    let settled = Record::PrivateEnd {
        at: private_at(),
        r#loop: loop_a,
        token: token_a,
        outcome: PrivateOutcome::Settled {
            is_error: false,
            parts: private_settled_parts(),
        },
    };
    let encoded = encode(&settled)?;
    let text = String::from_utf8(encoded.clone())?;
    assert!(text.starts_with(r#"{"v":1,"type":"private_end""#), "{text}");
    assert!(text.contains(r#""type":"settled""#), "{text}");
    assert_eq!(settled.tag(), "private_end");
    assert!(settled.entry().is_none());
    let decoded = decode(&encoded)?.record;
    assert_eq!(decoded, settled);
    assert_eq!(encode(&decoded)?, encoded);
    let Record::PrivateEnd {
        r#loop,
        token,
        outcome,
        ..
    } = &decoded
    else {
        return Err("expected a private end".into());
    };
    assert_eq!(*r#loop, loop_a);
    assert_eq!(*token, token_a);
    let PrivateOutcome::Settled { is_error, parts } = outcome else {
        return Err("expected settled parts".into());
    };
    assert!(!*is_error);
    assert_eq!(parts.as_slice(), private_settled_parts().as_slice());
    Ok(())
}

#[test]
fn private_records_round_trip_indeterminate() -> Result<(), Box<dyn std::error::Error>> {
    let (loop_a, token_a, _) = private_round_trip_ids();

    let open = Record::PrivateEnd {
        at: private_at(),
        r#loop: loop_a,
        token: token_a,
        outcome: PrivateOutcome::Indeterminate,
    };
    let encoded = encode(&open)?;
    let text = String::from_utf8(encoded.clone())?;
    assert!(text.contains(r#""type":"indeterminate""#), "{text}");
    assert!(!text.contains("\"parts\""), "{text}");
    let decoded = decode(&encoded)?.record;
    assert_eq!(decoded, open);
    assert_eq!(encode(&decoded)?, encoded);
    assert!(matches!(
        decoded,
        Record::PrivateEnd {
            outcome: PrivateOutcome::Indeterminate,
            ..
        }
    ));
    Ok(())
}

#[test]
fn private_turn_present_and_shared_call_across_loops() -> Result<(), Box<dyn std::error::Error>> {
    let loop_a = private_loop_id("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f");
    let loop_b = private_loop_id("01927f40-0000-7000-8000-000000000001");
    let token_a = private_token_id("01927f40-0000-7000-8000-000000000002");
    let token_b = private_token_id("01927f40-0000-7000-8000-000000000003");
    let turn = Some(TurnId::new(nz(1)));

    let loop_turned = Record::PrivateLoop {
        at: private_at(),
        r#loop: loop_a,
        who: private_owner(),
        turn,
    };
    let text = String::from_utf8(encode(&loop_turned)?)?;
    assert!(text.contains("\"turn\":1"), "{text}");
    assert_eq!(decode(text.as_bytes())?.record, loop_turned);

    let start_a = Record::PrivateStart {
        at: private_at(),
        r#loop: loop_a,
        token: token_a,
        who: private_owner(),
        turn,
        round: 0,
        call: CallId::new("toolu_01"),
        name: "read".into(),
        args: RawJson::parse(r#"{"path":"lib/lexer.ml"}"#)?,
    };
    let start_b = Record::PrivateStart {
        at: private_at(),
        r#loop: loop_b,
        token: token_b,
        who: private_owner(),
        turn,
        round: 1,
        call: CallId::new("toolu_01"),
        name: "read".into(),
        args: RawJson::parse(r#"{"path":"lib/parser.ml"}"#)?,
    };
    let decoded_a = decode(&encode(&start_a)?)?.record;
    let decoded_b = decode(&encode(&start_b)?)?.record;
    assert_eq!(decoded_a, start_a);
    assert_eq!(decoded_b, start_b);
    let (
        Record::PrivateStart {
            r#loop: got_a,
            token: tok_a,
            turn: turn_a,
            call: call_a,
            ..
        },
        Record::PrivateStart {
            r#loop: got_b,
            token: tok_b,
            turn: turn_b,
            call: call_b,
            ..
        },
    ) = (&decoded_a, &decoded_b)
    else {
        return Err("expected two private starts".into());
    };
    assert_ne!(*got_a, *got_b);
    assert_ne!(*tok_a, *tok_b);
    assert_eq!(*turn_a, turn);
    assert_eq!(*turn_b, turn);
    assert_eq!(call_a.as_str(), "toolu_01");
    assert_eq!(call_b.as_str(), "toolu_01");
    Ok(())
}

#[test]
fn private_outcome_rejects_unknown_and_malformed_shapes() {
    let head = "{\"v\":1,\"type\":\"private_end\",\"at\":\"2026-09-25T10:15:35.420Z\",\"loop\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"token\":\"01927f40-0000-7000-8000-000000000002\",\"outcome\":";
    let cases = [
        // Unknown outcome type.
        "{\"type\":\"queued\"}",
        // Settled without a required member.
        "{\"type\":\"settled\",\"parts\":[]}",
        "{\"type\":\"settled\",\"is_error\":false}",
        // Settled with an undeclared member.
        "{\"type\":\"settled\",\"is_error\":false,\"parts\":[],\"extra\":1}",
        // Indeterminate takes no other member.
        "{\"type\":\"indeterminate\",\"parts\":[]}",
        "{\"type\":\"indeterminate\",\"is_error\":false}",
        // Outcome without its type member.
        "{\"is_error\":false,\"parts\":[]}",
        // Missing and null outcomes.
        "null",
        "7",
        "\"settled\"",
    ];
    for outcome in cases {
        let line = format!("{head}{outcome}}}\n");
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    let missing = "{\"v\":1,\"type\":\"private_end\",\"at\":\"2026-09-25T10:15:35.420Z\",\"loop\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"token\":\"01927f40-0000-7000-8000-000000000002\"}\n";
    assert!(matches!(
        decode(missing.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    // An absent turn reads as None; a present null never does.
    let loop_head = "{\"v\":1,\"type\":\"private_loop\",\"at\":\"2026-09-25T10:15:35.420Z\",\"loop\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"who\":\"core\"";
    let absent = format!("{loop_head}}}\n");
    let decoded = decode(absent.as_bytes()).expect("absent turn is None");
    assert!(matches!(
        decoded.record,
        Record::PrivateLoop { turn: None, .. }
    ));
    let null_turn = format!("{loop_head},\"turn\":null}}\n");
    assert!(
        matches!(
            decode(null_turn.as_bytes()),
            Err(DecodeError::Invalid { .. })
        ),
        "{null_turn}"
    );
    let start_null_turn = "{\"v\":1,\"type\":\"private_start\",\"at\":\"2026-09-25T10:15:35.420Z\",\"loop\":\"01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f\",\"token\":\"01927f40-0000-7000-8000-000000000002\",\"who\":\"core\",\"turn\":null,\"round\":0,\"call\":\"toolu_01\",\"name\":\"read\",\"args\":{}}\n";
    assert!(matches!(
        decode(start_null_turn.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn private_records_reject_format_2_and_stay_non_tree() -> Result<(), Box<dyn std::error::Error>> {
    let loop_a = private_loop_id("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f");
    let token_a = private_token_id("01927f40-0000-7000-8000-000000000002");
    let records = [
        Record::PrivateLoop {
            at: private_at(),
            r#loop: loop_a,
            who: Owner::Core,
            turn: None,
        },
        Record::PrivateStart {
            at: private_at(),
            r#loop: loop_a,
            token: token_a,
            who: Owner::Core,
            turn: Some(TurnId::new(nz(1))),
            round: 0,
            call: CallId::new("toolu_01"),
            name: "read".into(),
            args: RawJson::parse(r"{}")?,
        },
        Record::PrivateEnd {
            at: private_at(),
            r#loop: loop_a,
            token: token_a,
            outcome: PrivateOutcome::Indeterminate,
        },
    ];
    for record in records {
        assert!(record.entry().is_none());
        let v1 = String::from_utf8(encode(&record)?)?;
        assert!(v1.starts_with("{\"v\":1,"), "{v1}");
        assert_eq!(scan_head(v1.as_bytes()), None);
        let v2 = v1.replacen("\"v\":1", "\"v\":2", 1);
        assert!(
            matches!(decode(v2.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{v2}"
        );
    }
    Ok(())
}

#[test]
fn branch_omits_private_non_tree_records() -> Result<(), Box<dyn std::error::Error>> {
    let header = Header {
        id: SessionId::new_v7(),
        at: "2026-09-25T10:15:30.000Z".parse()?,
        workspace: harness_workspace()?,
        product: Product::Dal,
        from: None,
    };
    let loop_a = private_loop_id("01927f3a-8c2e-7b4d-9f10-3a5b6c7d8e9f");
    let token_a = private_token_id("01927f40-0000-7000-8000-000000000002");
    let who = private_owner();
    let records = vec![
        Record::User(user_entry(1, None, "first")),
        Record::PrivateLoop {
            at: private_at(),
            r#loop: loop_a,
            who: who.clone(),
            turn: None,
        },
        Record::User(user_entry(2, Some(1), "second")),
        Record::PrivateStart {
            at: private_at(),
            r#loop: loop_a,
            token: token_a,
            who: who.clone(),
            turn: None,
            round: 0,
            call: CallId::new("toolu_01"),
            name: "read".into(),
            args: RawJson::parse(r#"{"path":"lib/lexer.ml"}"#)?,
        },
        Record::User(user_entry(3, Some(2), "third")),
        Record::PrivateEnd {
            at: private_at(),
            r#loop: loop_a,
            token: token_a,
            outcome: PrivateOutcome::Settled {
                is_error: false,
                parts: vec![JournalPart::Text { text: "ok".into() }],
            },
        },
        Record::Label {
            at: private_at(),
            entry: EntryId::new(nz(1)),
            label: Some("kept".into()),
        },
        Record::PrivateEnd {
            at: private_at(),
            r#loop: loop_a,
            token: token_a,
            outcome: PrivateOutcome::Indeterminate,
        },
    ];
    let is_private = |record: &Record| {
        matches!(
            record,
            Record::PrivateLoop { .. } | Record::PrivateStart { .. } | Record::PrivateEnd { .. }
        )
    };
    let forked = branch(
        &records,
        Some(EntryId::new(nz(3))),
        BranchMode::Fork {
            at: EntryId::new(nz(2)),
        },
        &header,
    )?;
    assert!(
        !forked.records.iter().any(is_private),
        "fork drops private audit records"
    );
    let ids: Vec<u64> = forked
        .records
        .iter()
        .filter_map(|record| record.entry().map(|entry| entry.id.get()))
        .collect();
    assert_eq!(ids, vec![1]);
    assert_eq!(
        forked.anchor_parts,
        vec![JournalPart::Text {
            text: "second".into()
        }]
    );
    let cloned = branch(
        &records,
        Some(EntryId::new(nz(3))),
        BranchMode::Clone,
        &header,
    )?;
    assert!(
        !cloned.records.iter().any(is_private),
        "clone drops private audit records"
    );
    let ids: Vec<u64> = cloned
        .records
        .iter()
        .filter_map(|record| record.entry().map(|entry| entry.id.get()))
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
    assert_eq!(cloned.anchor_parts, []);
    Ok(())
}

fn usage_with_premium(premium_requests: Option<f64>) -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests,
    }
}

fn premium_fixture_at() -> jiff::Timestamp {
    "2026-09-25T10:16:02.000Z"
        .parse()
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}

fn assistant_with_premium(premium_requests: Option<f64>) -> Record {
    let mut entry = assistant_entry("m", None);
    if let EntryKind::Assistant { usage, .. } = &mut entry.kind {
        usage.premium_requests = premium_requests;
    }
    Record::Assistant(entry)
}

fn inferred_with_premium(premium_requests: Option<f64>) -> Record {
    Record::Inferred {
        at: premium_fixture_at(),
        who: Owner::Core,
        purpose: InferredPurpose::Synthetic {
            id: "dalgona/fusion".into(),
        },
        usage: usage_with_premium(premium_requests),
    }
}

fn compaction_entry(usage: Option<Usage>) -> Record {
    Record::Compaction(Entry {
        id: EntryId::new(nz(58)),
        parent: Some(EntryId::new(nz(57))),
        at: premium_fixture_at(),
        kind: EntryKind::Compaction {
            summary: None,
            first_kept: None,
            tokens_before: 1,
            replay: None,
            usage,
            parts: Vec::new(),
            parts_tokens: 0,
        },
    })
}

fn compaction_with_premium(premium_requests: Option<f64>) -> Record {
    compaction_entry(Some(usage_with_premium(premium_requests)))
}

fn compaction_without_usage() -> Record {
    compaction_entry(None)
}

fn turn_end_with_premium(premium_requests: Option<f64>) -> Record {
    Record::TurnEnd {
        at: premium_fixture_at(),
        turn: TurnId::new(nz(1)),
        stop: TurnEndStop::Done,
        usage: Some(usage_with_premium(premium_requests)),
        changes: Vec::new(),
    }
}

fn decoded_premium(line: &str) -> Result<Option<f64>, Box<dyn std::error::Error>> {
    Ok(match decode(line.as_bytes())?.record {
        Record::Assistant(entry) => match entry.kind {
            EntryKind::Assistant { usage, .. } => usage.premium_requests,
            _ => return Err("expected an assistant entry".into()),
        },
        Record::Inferred { usage, .. } => usage.premium_requests,
        Record::Compaction(entry) => match entry.kind {
            EntryKind::Compaction { usage, .. } => usage.and_then(|inner| inner.premium_requests),
            _ => return Err("expected a compaction entry".into()),
        },
        Record::TurnEnd { usage, .. } => usage.and_then(|inner| inner.premium_requests),
        _ => return Err("expected a usage record".into()),
    })
}

#[test]
fn premium_requests_round_trip_each_record_family() -> Result<(), Box<dyn std::error::Error>> {
    for record in [
        assistant_with_premium(Some(2.5)),
        inferred_with_premium(Some(2.5)),
        compaction_with_premium(Some(2.5)),
        turn_end_with_premium(Some(2.5)),
    ] {
        let line = String::from_utf8(encode(&record)?)?;
        assert!(line.contains("\"premium_requests\":2.5"), "{line}");
        assert_eq!(decode(line.as_bytes())?.record, record);
    }
    Ok(())
}

#[test]
fn premium_none_preserves_legacy_bytes() -> Result<(), Box<dyn std::error::Error>> {
    for record in [
        assistant_with_premium(None),
        inferred_with_premium(None),
        compaction_with_premium(None),
        compaction_without_usage(),
        turn_end_with_premium(None),
    ] {
        let line = String::from_utf8(encode(&record)?)?;
        assert!(!line.contains("premium_requests"), "{line}");
        assert_eq!(decode(line.as_bytes())?.record, record);
    }
    Ok(())
}

#[test]
fn premium_absent_decodes_to_none() -> Result<(), Box<dyn std::error::Error>> {
    for line in [
        assistant_line("m", None),
        "{\"v\":1,\"type\":\"inferred\",\"at\":\"2026-09-25T10:16:02.000Z\",\"who\":\"core\",\"purpose\":{\"synthetic\":{\"id\":\"dalgona/fusion\"}},\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}\n".to_string(),
        "{\"v\":1,\"type\":\"compaction\",\"id\":58,\"parent\":57,\"at\":\"2026-09-25T10:16:02.000Z\",\"summary\":null,\"first_kept\":null,\"tokens_before\":1,\"replay\":null,\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null}}\n".to_string(),
        "{\"v\":1,\"type\":\"compaction\",\"id\":58,\"parent\":57,\"at\":\"2026-09-25T10:16:02.000Z\",\"summary\":null,\"first_kept\":null,\"tokens_before\":1,\"replay\":null,\"usage\":null}\n".to_string(),
        "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":null},\"changes\":[]}\n".to_string(),
    ] {
        assert_eq!(decoded_premium(&line)?, None, "{line}");
    }
    Ok(())
}

#[test]
fn premium_float_boundary_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let cases = [
        0.0,
        -0.0,
        0.000_000_4,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        1.0,
        f64::from_bits(1.0_f64.to_bits() + 1),
        0.1,
        1.0 / 3.0,
        f64::MAX,
    ];
    for value in cases {
        let record = turn_end_with_premium(Some(value));
        let line = String::from_utf8(encode(&record)?)?;
        assert!(line.contains("\"premium_requests\":"), "{line}");
        assert_eq!(decode(line.as_bytes())?.record, record);
        assert_eq!(decoded_premium(&line)?, Some(value));
    }
    Ok(())
}

#[test]
fn premium_encode_rejects_invalid_values() {
    for premium in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.000_001] {
        assert!(
            matches!(
                encode(&turn_end_with_premium(Some(premium))),
                Err(EncodeError::InvalidPremium)
            ),
            "{premium}"
        );
    }
}

#[test]
fn premium_decode_rejects_invalid_members() {
    let base = assistant_line("m", None);
    for raw in [
        "\"1\"",
        "\"2.5\"",
        "-1",
        "-0.000001",
        "-0.5",
        "true",
        "false",
        "null",
        "1e400",
        "1e500",
        "9e999",
    ] {
        let line = base.replace(
            "\"stop\":\"done\"}",
            &format!("\"stop\":\"done\",\"premium_requests\":{raw}}}"),
        );
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
    // The nested usage object stays closed: premium rides top-level only.
    let nested = base.replace(
        "\"cost_micro_usd\":null",
        "\"cost_micro_usd\":null,\"premium_requests\":1.0",
    );
    assert!(matches!(
        decode(nested.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[test]
fn premium_without_optional_usage_is_rejected() {
    let compaction = "{\"v\":1,\"type\":\"compaction\",\"id\":58,\"parent\":57,\"at\":\"2026-09-25T10:16:02.000Z\",\"summary\":null,\"first_kept\":null,\"tokens_before\":1,\"replay\":null,\"usage\":null,\"premium_requests\":1.0}\n";
    assert!(matches!(
        decode(compaction.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
    let turn_end = "{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":null,\"changes\":[],\"premium_requests\":1.0}\n";
    assert!(matches!(
        decode(turn_end.as_bytes()),
        Err(DecodeError::Invalid { .. })
    ));
}

#[cfg(unix)]
#[test]
fn canonical_vectors_encoded_len_matches_encode_len() -> Result<(), Box<dyn std::error::Error>> {
    for line in VECTORS {
        let decoded = decode(line.as_bytes())?;
        let encoded = encode(&decoded.record)?;
        let measured_len = encoded_len(&decoded.record)?;
        assert_eq!(measured_len, encoded.len(), "vector line: {line}");
    }
    Ok(())
}

#[test]
fn representative_records_encoded_len_matches_encode_len() -> Result<(), Box<dyn std::error::Error>>
{
    // Raw/non-ASCII replay
    let replay_raw = RawJson::parse(
        r#"{"api":"anthropic","text":"hello \n \t \u0000 🚀 \"world\"","nested":{"flag":true}}"#,
    )?;
    let raw_record = assistant_with_replay(Vec::new(), Some(replay_raw));
    let encoded = encode(&raw_record)?;
    let decoded = decode(&encoded)?;
    assert_eq!(decoded.record, raw_record);
    assert_eq!(encoded_len(&raw_record)?, encoded.len());

    // Escaped user_entry text
    let user_record = Record::User(user_entry(
        42,
        Some(1),
        "Line 1\nLine 2\t\r\"escaped quotes\" and backslash \\ and unicode \u{1F980} & \u{00E9}",
    ));
    let encoded = encode(&user_record)?;
    let decoded = decode(&encoded)?;
    assert_eq!(decoded.record, user_record);
    assert_eq!(encoded_len(&user_record)?, encoded.len());

    // v:2 turn_reserved
    let reserved_record = Record::TurnReserved {
        at: "2026-10-13T00:00:00.000Z"
            .parse()
            .map_err(|error: jiff::Error| error.to_string())?,
        turn: TurnId::new(nz(2)),
    };
    let encoded = encode(&reserved_record)?;
    let decoded = decode(&encoded)?;
    assert_eq!(decoded.record, reserved_record);
    assert_eq!(encoded_len(&reserved_record)?, encoded.len());

    // Routed assistant (API and synthetic)
    let api_routed = Record::Assistant(assistant_entry(
        "probe/model",
        Some(AssistantRoute::Api(Family::Chat)),
    ));
    let encoded = encode(&api_routed)?;
    let decoded = decode(&encoded)?;
    assert_eq!(decoded.record, api_routed);
    assert_eq!(encoded_len(&api_routed)?, encoded.len());

    let synthetic_routed = Record::Assistant(assistant_entry(
        "probe/model",
        Some(AssistantRoute::Synthetic),
    ));
    let encoded = encode(&synthetic_routed)?;
    let decoded = decode(&encoded)?;
    assert_eq!(decoded.record, synthetic_routed);
    assert_eq!(encoded_len(&synthetic_routed)?, encoded.len());

    Ok(())
}

#[test]
fn encoded_len_and_encode_error_parity() {
    // NullReplay: explicit null replay in assistant entry
    let null_replay = assistant_with_replay(Vec::new(), Some(RawJson::null()));
    let len_err = encoded_len(&null_replay);
    let enc_err = encode(&null_replay);
    assert_eq!(len_err, Err(EncodeError::NullReplay));
    assert_eq!(enc_err.map(|v| v.len()), len_err);

    // EmptyModel: empty model id on assistant entry
    let empty_model = Record::Assistant(assistant_entry("", None));
    let len_err = encoded_len(&empty_model);
    let enc_err = encode(&empty_model);
    assert_eq!(len_err, Err(EncodeError::EmptyModel));
    assert_eq!(enc_err.map(|v| v.len()), len_err);

    // InvalidRoute: synthetic grammar violation
    let invalid_route =
        Record::Assistant(assistant_entry("Upper/x", Some(AssistantRoute::Synthetic)));
    let len_err = encoded_len(&invalid_route);
    let enc_err = encode(&invalid_route);
    assert_eq!(
        len_err,
        Err(EncodeError::InvalidRoute(RouteError::InvalidSyntheticId {
            id: "Upper/x".into(),
        }))
    );
    assert_eq!(enc_err.map(|v| v.len()), len_err);

    // InvalidCost: negative dollar cost
    let invalid_cost = turn_end_with_cost(-0.000_001);
    let len_err = encoded_len(&invalid_cost);
    let enc_err = encode(&invalid_cost);
    assert_eq!(len_err, Err(EncodeError::InvalidCost));
    assert_eq!(enc_err.map(|v| v.len()), len_err);
}

fn provider_model_record(route: ModelRoute) -> Record {
    Record::Model(Entry {
        id: EntryId::new(nz(1)),
        parent: None,
        at: "2026-09-25T10:15:30.125Z"
            .parse()
            .expect("valid fixture timestamp"),
        kind: EntryKind::Model { route },
    })
}

#[test]
fn model_record_with_provider_round_trips_byte_exactly() -> Result<(), Box<dyn std::error::Error>> {
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6-luna".into(),
        provider: Some("acme".into()),
    };
    let record = provider_model_record(route.clone());
    let line = "{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",\"api\":\"openai_responses\",\"model\":\"gpt-6-luna\",\"provider\":\"acme\"}\n";
    let encoded = encode(&record)?;
    assert_eq!(encoded.as_slice(), line.as_bytes());
    assert_eq!(decode(line.as_bytes())?.record, record);
    assert_eq!(decoded_route(line)?, route);
    Ok(())
}

#[test]
fn provider_less_api_record_keeps_its_legacy_bytes() -> Result<(), Box<dyn std::error::Error>> {
    let record = provider_model_record(ModelRoute::Api {
        family: Family::Anthropic,
        model: "claude-opus".into(),
        provider: None,
    });
    let line = "{\"v\":1,\"type\":\"model\",\"id\":1,\"parent\":null,\"at\":\"2026-09-25T10:15:30.125Z\",\"api\":\"anthropic\",\"model\":\"claude-opus\"}\n";
    assert!(!line.contains("provider"));
    assert_eq!(encode(&record)?.as_slice(), line.as_bytes());
    assert_eq!(decode(line.as_bytes())?.record, record);
    assert_eq!(decoded_route(line)?.provider(), None);
    Ok(())
}

#[test]
fn model_record_rejects_bad_provider_members() {
    for tail in [
        r#""api":"anthropic","model":"m","provider":"""#,
        r#""api":"anthropic","model":"m","provider":null"#,
        r#""api":"anthropic","model":"m","provider":7"#,
        r#""api":"anthropic","model":"m","provider":true"#,
        r#""api":"anthropic","model":"m","provider":["acme"]"#,
        r#""route":{"synthetic":{"id":"a/b"}},"provider":"acme""#,
    ] {
        let line = model_line(tail);
        assert!(
            matches!(decode(line.as_bytes()), Err(DecodeError::Invalid { .. })),
            "{line}"
        );
    }
}

#[test]
fn encode_refuses_an_empty_provider_id() {
    let record = provider_model_record(ModelRoute::Api {
        family: Family::Chat,
        model: "m".into(),
        provider: Some("".into()),
    });
    assert!(matches!(encode(&record), Err(EncodeError::EmptyProvider)));
}

#[test]
fn synthetic_and_harness_model_records_never_carry_a_provider()
-> Result<(), Box<dyn std::error::Error>> {
    for route in [
        ModelRoute::synthetic("a/b").expect("valid fixture id"),
        ModelRoute::harness("dalgon/normal").expect("valid fixture id"),
    ] {
        assert_eq!(route.provider(), None);
        assert_eq!(route.with_provider("acme"), route);
        let record = provider_model_record(route);
        let encoded = encode(&record)?;
        let text = std::str::from_utf8(&encoded)?;
        assert!(!text.contains("provider"), "{text}");
        assert_eq!(decode(&encoded)?.record, record);
    }
    Ok(())
}

fn turn_end_line_with_micro(micro: &str) -> String {
    format!(
        "{{\"v\":1,\"type\":\"turn_end\",\"at\":\"2026-09-25T10:16:02.000Z\",\"turn\":1,\"stop\":\"done\",\"usage\":{{\"input\":1,\"output\":1,\"cache_read\":0,\"cache_write\":0,\"reasoning\":null,\"cost_micro_usd\":{micro}}},\"changes\":[]}}\n"
    )
}

/// A stored cost is whole micro-dollars read into an `f64` dollar amount.
/// Measured: every count through 2^51 (about $2.25 billion) comes back byte
/// for byte, and from 2^52 the float starts to round, so only the exact
/// region is pinned. The member keeps its unsigned-integer type: a negative,
/// fractional, exponent, quoted, boolean or past-`u64` spelling is refused at
/// its value, never rounded in.
#[test]
fn micro_dollar_costs_round_trip_byte_exactly_through_two_to_the_fifty_first()
-> Result<(), Box<dyn std::error::Error>> {
    for micro in [
        "null",
        "0",
        "1",
        "999999",
        "1000000",
        "1125899906842623",
        "1125899906842624",
        "2251799813685247",
        "2251799813685248",
        "2251799813685249",
    ] {
        let line = turn_end_line_with_micro(micro);
        let decoded = decode(line.as_bytes())?;
        assert_eq!(
            encode(&decoded.record)?.as_slice(),
            line.as_bytes(),
            "{micro}"
        );
    }
    for bad in [
        "18446744073709551616",
        "-1",
        "1.5",
        "1e3",
        "\"5\"",
        "true",
        "[]",
    ] {
        let line = turn_end_line_with_micro(bad);
        let marker = "\"cost_micro_usd\":";
        let expected = line.find(marker).map(|at| at + marker.len());
        assert!(
            matches!(
                decode(line.as_bytes()),
                Err(DecodeError::Invalid { offset, .. }) if Some(offset) == expected
            ),
            "{bad}"
        );
    }
    Ok(())
}

/// Bytes a corrupting edit may write: digits and signs, exponent and null
/// spellings, structure characters, an escape and a NUL.
#[cfg(unix)]
const CORRUPTING_BYTES: &[u8] = b"0123456789-+.eEnul\"\\,:{}[] \t/\x00truefalsenull";

#[cfg(unix)]
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(4000))]

    /// The decoder is the only gate a stored line passes. Every line it
    /// accepts after one to three corrupting edits of a canonical vector must
    /// encode, the encoded line must decode, and encoding that again must give
    /// the same bytes: the writer normalizes (millisecond timestamps, dropped
    /// unknown members) once and then holds still. A refusal or a drifting
    /// second encoding means the reader accepted what the writer cannot keep.
    #[test]
    fn a_line_the_decoder_accepts_re_encodes_to_a_fixed_point(
        index in 0..VECTORS.len(),
        edits in proptest::collection::vec(
            (0_usize..4096, 0_u8..3, proptest::sample::select(CORRUPTING_BYTES.to_vec())),
            1..4,
        ),
    ) {
        let mut line = VECTORS[index].as_bytes().to_vec();
        for (at, kind, byte) in edits {
            if line.is_empty() {
                break;
            }
            let at = at % line.len();
            match kind {
                0 => {
                    line.remove(at);
                }
                1 => line[at] = byte,
                _ => line.insert(at, byte),
            }
        }
        let Ok(decoded) = decode(&line) else {
            return Ok(());
        };
        let shown = || String::from_utf8_lossy(&line).into_owned();
        let first = encode(&decoded.record).map_err(|error| {
            proptest::test_runner::TestCaseError::fail(format!(
                "the decoder accepted {} but the encoder refuses it: {error:?}",
                shown()
            ))
        })?;
        let again = decode(&first).map_err(|error| {
            proptest::test_runner::TestCaseError::fail(format!(
                "the encoder wrote a line the decoder refuses ({error:?}) from {}",
                shown()
            ))
        })?;
        let second = encode(&again.record).map_err(|error| {
            proptest::test_runner::TestCaseError::fail(format!(
                "the second encoding failed ({error:?}) from {}",
                shown()
            ))
        })?;
        proptest::prop_assert_eq!(second, first, "from {}", shown());
    }
}
