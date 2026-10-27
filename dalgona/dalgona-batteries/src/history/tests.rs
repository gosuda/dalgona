// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History battery behavior tests.

use std::num::NonZeroU64;
use std::sync::Arc;

use dal_agent::ext::{CompactInput, CoveredEntry, Services, ToolCx};
use dal_core::{ContextItem, EntryId, Family, ModelRoute, Part, RequestParams, SessionId};

use super::config::{HistoryConfig, HistoryConfigError, parse_config};
use super::draw::{Grid, draw, paginate};
use super::dream::{
    DreamAction, DreamEvent, DreamFileError, DreamPhase, DreamState, check_dream_file,
    consolidated_notice, decode_dream_file, failed_notice, park_notice, transition,
};
use super::pipeline::letter_text;
use super::records::{LetterRecord, RecordError};
use super::selection::{
    LetterVisibility, history_index_line, select_oldest_plus_newest,
    select_oldest_plus_newest_by_bytes,
};
use super::spans::{
    CompactPiece, HistoryError, Item, Role, SourceError, Span, items, retained_segments,
};
use super::wiring::history;
use crate::work::support::FakeServices;

fn entry_id(value: u64) -> Option<EntryId> {
    NonZeroU64::new(value).map(EntryId::new)
}

fn table(toml: &str) -> Option<toml::Value> {
    let value: toml::Value = toml::from_str(toml).expect("fixture TOML parses");
    value.get("plugin")?.get("history").cloned()
}

#[test]
fn config_defaults_to_enabled_share() {
    assert_eq!(
        parse_config(None),
        Ok(HistoryConfig::Enabled { share: 0.4 })
    );
    assert_eq!(
        parse_config(table("[plugin.history]").as_ref()),
        Ok(HistoryConfig::Enabled { share: 0.4 })
    );
}

#[test]
fn config_rejects_invalid_enabled_share_and_unknown_key() {
    let enabled = table("[plugin.history]\nenabled = \"yes\"\n");
    assert_eq!(
        parse_config(enabled.as_ref()),
        Err(HistoryConfigError::Enabled)
    );
    let share_type = table("[plugin.history]\nshare = \"lots\"\n");
    assert_eq!(
        parse_config(share_type.as_ref()),
        Err(HistoryConfigError::Share)
    );
    let share_low = table("[plugin.history]\nshare = 0.05\n");
    assert_eq!(
        parse_config(share_low.as_ref()),
        Err(HistoryConfigError::Share)
    );
    let share_high = table("[plugin.history]\nshare = 0.9\n");
    assert_eq!(
        parse_config(share_high.as_ref()),
        Err(HistoryConfigError::Share)
    );
    let unknown = table("[plugin.history]\nunknown = true\n");
    assert_eq!(
        parse_config(unknown.as_ref()),
        Err(HistoryConfigError::UnknownKey("unknown".to_string()))
    );
}

#[test]
fn config_disables_on_false() {
    let disabled = table("[plugin.history]\nenabled = false\n");
    assert_eq!(parse_config(disabled.as_ref()), Ok(HistoryConfig::Disabled));
}

#[test]
fn disabled_registers_no_compactor_or_hooks() {
    let extension =
        history(HistoryConfig::Disabled).expect("a disabled section still builds the extension");
    assert!(
        extension.compactors().is_empty(),
        "disabled history wires no compactor"
    );
    assert!(
        extension.session_setup().is_none(),
        "disabled history wires no consolidation task"
    );
}

fn covered_user(entry: u64, text: &str) -> CoveredEntry {
    CoveredEntry {
        entry: entry_id(entry).expect("nonzero entry"),
        starts_user_turn: true,
        estimated_tokens: 1_000,
        content: ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        },
        note: None,
    }
}

#[tokio::test]
async fn invalid_config_installs_only_a_compactor_that_refuses_with_its_exact_message() {
    let message = HistoryConfigError::UnknownKey("bogus".to_owned()).to_string();
    let invalid = history(HistoryConfig::Invalid {
        message: message.as_str().into(),
    })
    .expect("an invalid section still builds the history extension");
    let enabled = history(HistoryConfig::Enabled { share: 0.4 }).expect("enabled history builds");

    let names: Vec<&str> = invalid
        .compactors()
        .iter()
        .map(|(name, _)| &**name)
        .collect();
    assert_eq!(names, ["history"]);
    assert!(
        invalid.session_setup().is_none(),
        "an invalid section wires no consolidation task"
    );
    assert!(
        enabled.session_setup().is_some(),
        "a valid section wires the consolidation task"
    );

    let services: Arc<dyn Services> = Arc::new(FakeServices::default());
    let cx = ToolCx::for_test(Arc::clone(&services));
    let entries = [
        covered_user(1, &"a".repeat(300)),
        covered_user(2, &"b".repeat(40)),
    ];
    let input = CompactInput {
        caller: cx.caller(),
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "test".into(),
            provider: None,
        },
        session: SessionId::new_v7(),
        instructions: "",
        covered: &entries,
        span: (
            entry_id(1).expect("nonzero entry"),
            entry_id(2).expect("nonzero entry"),
        ),
        first_kept: None,
        context_window: Some(200_000),
        image_profile: None,
        images_elsewhere: 0,
        image_bytes_elsewhere: 0,
        carried: None,
        total_tokens: 2_000,
        params: RequestParams::default(),
    };
    let (_, compactor) = &invalid.compactors()[0];
    let Err(refusal) = compactor.compact(input, services).await else {
        panic!("an invalid section must refuse every compaction");
    };
    assert_eq!(refusal.to_string(), message);
}

#[test]
fn record_rejects_letter_span_and_cell_caps() {
    let entry = entry_id(10).expect("nonzero entry");
    let span = Span {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 0,
        len: 1,
    };
    let many_letters = LetterRecord::Compaction {
        v: 1,
        id: "history/1.1".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 10,
        width: 100,
        height: 100,
        cell: [11, 16],
        spans: vec![span],
        letters: vec![1; 4097],
    };
    assert_eq!(
        LetterRecord::check(&many_letters, entry),
        Err(RecordError::TooManyLetters { count: 4097 })
    );
    let many_spans = LetterRecord::Compaction {
        v: 1,
        id: "history/1.1".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 10,
        width: 100,
        height: 100,
        cell: [11, 16],
        spans: vec![span; 65_537],
        letters: vec![1],
    };
    assert_eq!(
        LetterRecord::check(&many_spans, entry),
        Err(RecordError::TooManySpans { count: 65_537 })
    );
    let bad_cell = LetterRecord::Compaction {
        v: 1,
        id: "history/1.1".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 10,
        width: 100,
        height: 100,
        cell: [64, 65],
        spans: vec![],
        letters: vec![],
    };
    assert_eq!(
        LetterRecord::check(&bad_cell, entry),
        Err(RecordError::BadCell {
            width: 64,
            height: 65
        })
    );
    let bad_span = Span {
        entry,
        part: 0,
        off: 0,
        len: 1,
    };
    let bad_record = LetterRecord::Compaction {
        v: 1,
        id: "history/1.1".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 10,
        width: 100,
        height: 100,
        cell: [11, 16],
        spans: vec![bad_span],
        letters: vec![],
    };
    assert_eq!(
        LetterRecord::check(&bad_record, entry),
        Err(RecordError::BadSpan {
            entry: 10,
            part: 0,
            off: 0,
            len: 1
        })
    );
    assert_eq!(bad_record.spans(), &[bad_span]);
    assert_eq!(
        LetterRecord::Skill {
            v: 1,
            id: "name".to_string(),
            png_blob: "b".repeat(64),
            png_bytes: 4,
            width: 8,
            height: 8,
            cell: [8, 8],
            spans: vec![],
        }
        .spans(),
        &[]
    );
}

fn cap_entry() -> EntryId {
    entry_id(10).expect("nonzero entry")
}

fn cap_span(earlier: u64, len: u32) -> Span {
    Span {
        entry: entry_id(earlier).expect("nonzero"),
        part: 0,
        off: 0,
        len,
    }
}

fn cap_compaction(spans: Vec<Span>, letters: usize, cell: [u8; 2], blob: String) -> LetterRecord {
    LetterRecord::Compaction {
        v: 1,
        id: "history/1.1".to_string(),
        png_blob: blob,
        png_bytes: 10,
        width: 100,
        height: 100,
        cell,
        spans,
        letters: vec![1; letters],
    }
}

fn cap_digest() -> String {
    "a".repeat(64)
}

#[test]
fn a_compaction_record_holds_4096_letters_and_refuses_4097() {
    let letters = |count| cap_compaction(vec![cap_span(1, 1)], count, [11, 16], cap_digest());
    assert_eq!(LetterRecord::check(&letters(0), cap_entry()), Ok(()));
    assert_eq!(LetterRecord::check(&letters(4095), cap_entry()), Ok(()));
    assert_eq!(LetterRecord::check(&letters(4096), cap_entry()), Ok(()));
    assert_eq!(
        LetterRecord::check(&letters(4097), cap_entry()),
        Err(RecordError::TooManyLetters { count: 4097 })
    );
}

#[test]
fn a_dream_record_holds_4096_letters_and_refuses_4097() {
    let dream = |letters: usize| LetterRecord::Dream {
        v: 1,
        id: "dream/1".to_string(),
        letters: vec!["history/1.1".to_string(); letters],
        summary: "s".to_string(),
    };
    assert_eq!(LetterRecord::check(&dream(4096), cap_entry()), Ok(()));
    assert_eq!(
        LetterRecord::check(&dream(4097), cap_entry()),
        Err(RecordError::TooManyLetters { count: 4097 })
    );
}

#[test]
fn a_compaction_record_holds_65536_spans_and_refuses_65537() {
    let spans = |count| cap_compaction(vec![cap_span(1, 1); count], 1, [11, 16], cap_digest());
    assert_eq!(LetterRecord::check(&spans(65_535), cap_entry()), Ok(()));
    assert_eq!(LetterRecord::check(&spans(65_536), cap_entry()), Ok(()));
    assert_eq!(
        LetterRecord::check(&spans(65_537), cap_entry()),
        Err(RecordError::TooManySpans { count: 65_537 })
    );
}

#[test]
fn a_cell_side_is_one_through_sixty_four() {
    let with_cell = |cell| cap_compaction(vec![], 0, cell, cap_digest());
    for cell in [[1, 1], [64, 64], [1, 64], [64, 1]] {
        assert_eq!(
            LetterRecord::check(&with_cell(cell), cap_entry()),
            Ok(()),
            "{cell:?}"
        );
    }
    for cell in [[0, 16], [11, 0], [65, 16], [11, 65], [0, 0], [255, 255]] {
        assert_eq!(
            LetterRecord::check(&with_cell(cell), cap_entry()),
            Err(RecordError::BadCell {
                width: cell[0],
                height: cell[1]
            }),
            "{cell:?}"
        );
    }
}

#[test]
fn a_span_must_name_bytes_of_a_strictly_earlier_entry() {
    let with_span =
        |earlier, len| cap_compaction(vec![cap_span(earlier, len)], 0, [11, 16], cap_digest());
    assert_eq!(
        LetterRecord::check(&with_span(9, 1), cap_entry()),
        Ok(()),
        "a span may name the entry just before the record"
    );
    assert_eq!(LetterRecord::check(&with_span(1, 1), cap_entry()), Ok(()));
    for (earlier, len) in [(10, 1), (11, 1), (u64::MAX, 1), (9, 0), (1, 0)] {
        assert_eq!(
            LetterRecord::check(&with_span(earlier, len), cap_entry()),
            Err(RecordError::BadSpan {
                entry: earlier,
                part: 0,
                off: 0,
                len
            }),
            "entry {earlier} len {len}"
        );
    }
}

#[test]
fn a_png_digest_is_exactly_sixty_four_lowercase_hex_digits() {
    let with_blob = |blob: String| cap_compaction(vec![], 0, [11, 16], blob);
    let hex = "0123456789abcdef".repeat(4);
    assert_eq!(LetterRecord::check(&with_blob(hex), cap_entry()), Ok(()));
    for bad in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        format!("{}g", "a".repeat(63)),
        String::new(),
    ] {
        assert_eq!(
            LetterRecord::check(&with_blob(bad.clone()), cap_entry()),
            Err(RecordError::BadDigest {
                digest: bad.clone()
            }),
            "{bad:?}"
        );
    }
}

#[test]
fn record_decode_rejects_unknown_member_and_version() {
    let entry = entry_id(5).expect("nonzero entry");
    let unknown = dal_core::RawJson::parse(
        r#"{"v":1,"id":"history/1.1","kind":"compaction","png_blob":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","png_bytes":10,"width":100,"height":100,"cell":[11,16],"spans":[],"letters":[],"extra":1}"#,
    )
    .expect("valid JSON");
    assert_eq!(
        LetterRecord::decode(&unknown, entry),
        Err(RecordError::Decode)
    );
    let version = dal_core::RawJson::parse(
        r#"{"v":2,"id":"history/1.1","kind":"compaction","png_blob":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","png_bytes":10,"width":100,"height":100,"cell":[11,16],"spans":[],"letters":[]}"#,
    )
    .expect("valid JSON");
    assert_eq!(
        LetterRecord::decode(&version, entry),
        Err(RecordError::Decode)
    );
}

#[test]
fn dream_parks_on_repeated_failures_and_resets() {
    let mut state = DreamState::new();
    for _ in 0..2 {
        let actions = transition(
            &mut state,
            DreamEvent::JobIdenticalFailure("bad".to_string()),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(state.phase, DreamPhase::Idle);
    }
    let actions = transition(
        &mut state,
        DreamEvent::JobIdenticalFailure("bad".to_string()),
    );
    assert_eq!(actions.len(), 1);
    assert_eq!(state.phase, DreamPhase::Parked);
    let probe = transition(&mut state, DreamEvent::ProbeFailed);
    assert_eq!(probe.len(), 1);
    assert_eq!(state.phase, DreamPhase::Parked);
    transition(&mut state, DreamEvent::ProbeSucceeded);
    assert_eq!(state.identical_streak, 0);
    assert_eq!(state.transient_streak, 0);
    assert_eq!(state.phase, DreamPhase::Idle);
}

#[test]
fn probe_that_ends_neutrally_returns_to_parked_with_streaks_kept() {
    let mut state = DreamState::new();
    for _ in 0..6 {
        transition(&mut state, DreamEvent::JobTransientFailure);
    }
    assert_eq!(state.phase, DreamPhase::Parked);
    state.phase = DreamPhase::Probing;
    state.running = true;
    assert_eq!(transition(&mut state, DreamEvent::JobCancelled).len(), 0);
    assert_eq!(state.phase, DreamPhase::Parked);
    assert!(!state.running);
    assert_eq!(state.transient_streak, 6);
    state.phase = DreamPhase::Probing;
    assert_eq!(transition(&mut state, DreamEvent::ProbeSucceeded).len(), 0);
    assert_eq!(state.phase, DreamPhase::Idle);
    assert_eq!(state.transient_streak, 0);
    assert!(!state.park_notified);
}

/// Builds a `dream.json` body over the canonical table, varying only `top_extra`
/// (a raw member fragment like `,"extra":1`) and `park_extra` inside `park`.
fn dream_file_body(top_extra: &str, park_extra: &str) -> Vec<u8> {
    format!(
        "{{\"v\":1,\"session\":\"0192aa\",\"last_consolidated_letter\":7,\"unreflected\":3,\
         \"park\":{{\"parked\":false,\"identical_streak\":0,\"transient_streak\":0,\
         \"last_probe_at\":null,\"last_failure\":null{park_extra}}},\
         \"updated_at\":\"2026-09-25T10:20:00.000Z\"{top_extra}}}"
    )
    .into_bytes()
}

#[test]
fn dream_neutral_events_keep_streaks() {
    let mut state = DreamState::new();
    state.running = true;
    state.phase = DreamPhase::Running;
    state.transient_streak = 2;
    assert_eq!(transition(&mut state, DreamEvent::JobCancelled).len(), 0);
    assert!(!state.running);
    assert_eq!(state.phase, DreamPhase::Idle);
    assert_eq!(state.transient_streak, 2);
    state.running = true;
    state.phase = DreamPhase::Running;
    assert_eq!(transition(&mut state, DreamEvent::BudgetExhausted).len(), 0);
    assert!(!state.running);
    assert_eq!(state.transient_streak, 2);
}

#[test]
fn dream_file_decode_names_unknown_members() {
    let valid = decode_dream_file(&dream_file_body("", "")).expect("canonical body decodes");
    assert_eq!(valid.last_consolidated_letter, 7);
    assert_eq!(valid.unreflected, 3);
    assert_eq!(
        decode_dream_file(&dream_file_body(",\"extra\":1", "")),
        Err(DreamFileError::UnknownKey {
            key: "extra".to_string()
        })
    );
    assert_eq!(
        decode_dream_file(&dream_file_body("", ",\"x\":1")),
        Err(DreamFileError::UnknownKey {
            key: "x".to_string()
        })
    );
    assert_eq!(
        decode_dream_file(b"not json"),
        Err(DreamFileError::Malformed)
    );
    assert_eq!(decode_dream_file(b"[1,2]"), Err(DreamFileError::Malformed));
}

#[test]
fn dream_file_check_rejects_version_and_session() {
    let valid = decode_dream_file(&dream_file_body("", "")).expect("canonical body decodes");
    assert_eq!(
        check_dream_file(&valid, "other"),
        Err(DreamFileError::Session {
            other: "0192aa".to_string()
        })
    );
    assert_eq!(check_dream_file(&valid, "0192aa"), Ok(()));
    let wrong_version = dream_file_body("", "")
        .into_iter()
        .enumerate()
        .map(|(index, byte)| if index == 5 { b'2' } else { byte })
        .collect::<Vec<u8>>();
    let decoded = decode_dream_file(&wrong_version).expect("version 2 parses");
    assert_eq!(
        check_dream_file(&decoded, "0192aa"),
        Err(DreamFileError::Version { found: 2 })
    );
}

#[test]
fn record_rejects_skill_spans_digest_and_dream_caps() {
    let entry = entry_id(10).expect("nonzero entry");
    let span = Span {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 0,
        len: 1,
    };
    let skill_spans = LetterRecord::Skill {
        v: 1,
        id: "name".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 10,
        width: 100,
        height: 100,
        cell: [11, 16],
        spans: vec![span],
    };
    assert_eq!(
        LetterRecord::check(&skill_spans, entry),
        Err(RecordError::SkillSpans { count: 1 })
    );
    for digest in ["A".repeat(64), "ab".to_string(), "g".repeat(64)] {
        let bad = LetterRecord::Skill {
            v: 1,
            id: "name".to_string(),
            png_blob: digest.clone(),
            png_bytes: 10,
            width: 100,
            height: 100,
            cell: [11, 16],
            spans: vec![],
        };
        assert_eq!(
            LetterRecord::check(&bad, entry),
            Err(RecordError::BadDigest { digest })
        );
    }
    let dream_many = LetterRecord::Dream {
        v: 1,
        id: "dream/1".to_string(),
        letters: vec!["history/1.1".to_string(); 4097],
        summary: "s".to_string(),
    };
    assert_eq!(
        LetterRecord::check(&dream_many, entry),
        Err(RecordError::TooManyLetters { count: 4097 })
    );
}

#[test]
fn config_rejects_integer_share() {
    let integer = table("[plugin.history]\nshare = 1\n");
    assert_eq!(
        parse_config(integer.as_ref()),
        Err(HistoryConfigError::Share)
    );
}

#[test]
fn config_accepts_share_endpoints_and_rejects_nonfinite_share() {
    for (text, share) in [("share = 0.1\n", 0.1), ("share = 0.7\n", 0.7)] {
        let value = table(&format!("[plugin.history]\n{text}"));
        assert_eq!(
            parse_config(value.as_ref()),
            Ok(HistoryConfig::Enabled { share })
        );
    }
    let nonfinite = table("[plugin.history]\nshare = nan\n");
    assert_eq!(
        parse_config(nonfinite.as_ref()),
        Err(HistoryConfigError::Share)
    );
}

#[test]
fn spans_truncate_tool_text_and_elide_base64() {
    let long = format!("x=data;base64,{}!tail", "A".repeat(300));
    let role = Role::Output("tool".into());
    let segments = retained_segments(&long, &role).expect("segments");
    assert!(
        segments
            .iter()
            .any(|segment| matches!(segment, super::spans::Retained::Gap(_)))
    );
    let tool_long = "b".repeat(2500);
    let segments = retained_segments(&tool_long, &role).expect("segments");
    assert!(
        segments
            .iter()
            .any(|segment| matches!(segment, super::spans::Retained::Gap(_)))
    );
}

#[test]
fn spans_items_aborts_on_fetch_failure() {
    let piece = CompactPiece {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 0,
        len: 5,
        total: 5,
        role: Role::User,
        picture: None,
    };
    let result = items(&[piece], |_| {
        Err(SourceError {
            message: "gone".to_string(),
        })
    });
    assert_eq!(
        result,
        Err(HistoryError::Fetch {
            entry: 1,
            message: "gone".to_string()
        })
    );
}

#[test]
fn selection_keeps_oldest_plus_newest() {
    let kept = select_oldest_plus_newest(25, |index| index == 0 || index >= 10);
    let expected: Vec<usize> = std::iter::once(0).chain(10..25).collect();
    assert_eq!(kept, expected);
    assert_eq!(select_oldest_plus_newest(0, |_| true).len(), 0);
    assert_eq!(select_oldest_plus_newest(3, |_| false), Vec::<usize>::new());
}

#[test]
fn selection_trims_images_at_the_byte_budget_boundary() {
    let sizes = [500, 200, 200];
    assert_eq!(select_oldest_plus_newest_by_bytes(&sizes, 899), vec![0, 2]);
    assert_eq!(
        select_oldest_plus_newest_by_bytes(&sizes, 900),
        vec![0, 1, 2]
    );
    assert_eq!(
        select_oldest_plus_newest_by_bytes(&[usize::MAX, 1], usize::MAX),
        vec![0]
    );
}

#[test]
fn history_index_lines_distinguish_visibility_states() {
    let drawn = history_index_line("history/2.3", 4, 7, LetterVisibility::Drawn);
    assert!(drawn.starts_with("letter://history/2.3  "));
    assert!(drawn.ends_with("entries 4-7"));
    let text = history_index_line("history/2.3", 4, 7, LetterVisibility::ShownAsText);
    assert!(text.ends_with("entries 4-7, shown as text"));
    assert_ne!(text, drawn);
    let hidden = history_index_line("history/2.3", 4, 7, LetterVisibility::NotDrawn);
    assert!(hidden.ends_with("entries 4-7, not drawn"));
    assert_ne!(hidden, drawn);
    assert_ne!(hidden, text);
}

#[test]
fn role_marks_cover_all_journal_roles() {
    let roles = [
        Role::User,
        Role::Assistant,
        Role::Call("tool".into()),
        Role::Output("tool".into()),
        Role::FailedOutput("tool".into()),
        Role::Note,
        Role::Reasoning,
    ];
    let marks: Vec<Option<Box<str>>> = roles.iter().map(Role::mark).collect();
    let texts: Vec<Option<&str>> = marks.iter().map(Option::as_deref).collect();
    assert_eq!(
        texts,
        [
            Some("¶user: "),
            Some("¶ai: "),
            Some("¶call:tool "),
            Some("¶out:tool "),
            Some("¶failed:tool "),
            Some("¶note: "),
            None,
        ]
    );
    let distinct: std::collections::BTreeSet<_> = texts.iter().flatten().collect();
    assert_eq!(distinct.len(), 6, "no two roles may share a mark");
}

#[test]
fn spans_fetch_each_retained_span_separately() {
    let text = "t".repeat(2500);
    let piece = CompactPiece {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 100,
        len: 2500,
        total: 2500,
        role: Role::Output("tool".into()),
        picture: None,
    };
    let mut seen: Vec<(u32, u32)> = Vec::new();
    let result = items(&[piece], |span| {
        seen.push((span.off, span.len));
        let start = usize::try_from(span.off - 100).expect("span inside piece");
        let end = start + usize::try_from(span.len).expect("span inside piece");
        Ok(text.as_bytes()[start..end].to_vec())
    })
    .expect("items build");
    assert_eq!(seen, vec![(100, 2500), (100, 1200), (1800, 800)]);
    let texts: Vec<&str> = result
        .iter()
        .filter_map(|item| match item {
            Item::Text { text, .. } => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0], &text.as_str()[..1200]);
    assert_eq!(texts[1], &text.as_str()[1700..]);
}

#[test]
fn spans_empty_text_emits_only_its_mark() {
    let piece = CompactPiece {
        entry: entry_id(2).expect("nonzero"),
        part: 0,
        off: 0,
        len: 0,
        total: 0,
        role: Role::User,
        picture: None,
    };
    let result = items(&[piece], |_| Ok(Vec::new())).expect("empty builds");
    assert_eq!(result, vec![Item::Mark("¶user: ".into())]);
}

#[test]
fn spans_combined_gaps_keep_journal_order() {
    let tool = format!(
        "head;base64,{}!mid{}tail",
        "A".repeat(300),
        "m".repeat(2400)
    );
    let piece = CompactPiece {
        entry: entry_id(3).expect("nonzero"),
        part: 0,
        off: 0,
        len: u32::try_from(tool.len()).expect("fixture fits u32"),
        total: u32::try_from(tool.len()).expect("fixture fits u32"),
        role: Role::Output("tool".into()),
        picture: None,
    };
    let result = items(&[piece], |span| {
        let start = usize::try_from(span.off).expect("span inside piece");
        let end = start + usize::try_from(span.len).expect("span inside piece");
        Ok(tool.as_bytes()[start..end].to_vec())
    })
    .expect("items build");
    let gaps = result
        .iter()
        .filter(|item| matches!(item, Item::Mark(mark) if mark.starts_with("¶gap:")))
        .count();
    assert_eq!(gaps, 2);
    let mut saw_text = 0;
    let mut saw_gap = 0;
    for item in &result {
        match item {
            Item::Text { .. } => saw_text += 1,
            Item::Mark(mark) if mark.starts_with("¶gap:") => saw_gap += 1,
            _ => {}
        }
    }
    assert_eq!((saw_text, saw_gap), (3, 2));
}

/// Deterministic xorshift64* generator: the contiguity property needs a
/// seeded stream without adding a property-test dependency.
struct SeedStream(u64);

impl SeedStream {
    fn next(&mut self) -> u64 {
        let mut word = self.0;
        word ^= word >> 12;
        word ^= word << 25;
        word ^= word >> 27;
        self.0 = word;
        word.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Every emitted text span must be ordered, non-overlapping, placed on
/// UTF-8 boundaries, and byte-exact against the journal fixture: the
/// concatenated span bytes equal the source bytes. The generator covers
/// UTF-8 ASCII, CRLF, tabs, CJK, Hangul, U+0301, U+200D, ESC sequences,
/// and non-BMP characters, shaped into lines 4..64 columns wide and
/// 2..16 rows tall. This test checks only the uncut whole-span path:
/// the 1800-byte cap keeps tool text under the truncation gate, so the
/// truncation and base64 elision cuts are pinned instead by the separate
/// fixed-input test below.
#[test]
fn span_contiguity_property() {
    const PIECES: [&str; 14] = [
        "a",
        "Z",
        "0",
        " ",
        "\r\n",
        "\t",
        "中",
        "한",
        "é",
        "\u{301}",
        "\u{200d}",
        "\u{1b}",
        "\u{1f600}",
        "~",
    ];
    let mut stream = SeedStream(0x1234_5678_9ABC_DEF0);
    for round in 0..256u64 {
        let cols = 4 + stream.below(61);
        let rows = 2 + stream.below(15);
        let mut source = String::new();
        for _ in 0..rows {
            for _ in 0..cols {
                let width = u64::try_from(PIECES.len()).expect("pieces fit u64");
                let pick = usize::try_from(stream.below(width)).expect("piece index fits usize");
                source.push_str(PIECES[pick]);
            }
            if stream.below(3) > 0 {
                source.push('\n');
            }
        }
        while source.len() > 1800 {
            source.pop();
        }
        let role = match round % 5 {
            0 => Role::User,
            1 => Role::Assistant,
            2 => Role::Note,
            3 => Role::Call("tool".into()),
            _ => Role::Output("tool".into()),
        };
        let bytes = source.as_bytes();
        let byte_len = u32::try_from(bytes.len()).expect("fixture fits u32");
        let piece = CompactPiece {
            entry: entry_id(1000 + round).expect("nonzero"),
            part: 0,
            off: 0,
            len: byte_len,
            total: byte_len,
            role,
            picture: None,
        };
        let built = items(std::slice::from_ref(&piece), |span| {
            let start = usize::try_from(span.off).expect("span inside piece");
            let end = start + usize::try_from(span.len).expect("span inside piece");
            Ok(bytes[start..end].to_vec())
        })
        .expect("items build");
        let mut cursor = 0usize;
        let mut joined = Vec::new();
        for item in &built {
            let Item::Text {
                span, total, text, ..
            } = item
            else {
                continue;
            };
            assert_eq!(*total, byte_len);
            let start = usize::try_from(span.off).expect("span offset fits");
            let end = start + usize::try_from(span.len).expect("span length fits");
            assert!(source.is_char_boundary(start), "span starts on a boundary");
            assert!(source.is_char_boundary(end), "span ends on a boundary");
            assert!(start >= cursor, "spans are ordered and non-overlapping");
            cursor = end;
            joined.extend_from_slice(text.as_bytes());
        }
        assert_eq!(
            joined, bytes,
            "concatenated span bytes equal the source bytes"
        );
    }
}

/// Truncation and base64 cuts keep the contiguity contract: the head is a
/// UTF-8-safe prefix of at most 1200 bytes, the tail resumes on a
/// boundary, and every emitted span stays ordered and non-overlapping.
#[test]
fn span_truncation_and_elision_cuts_keep_contiguity() {
    let head_pad = "中".repeat(400);
    let tail_pad = "한".repeat(300);
    // One ASCII byte first so byte 1200 lands inside a 3-byte character
    // and the 1200-byte head cut must step back through utf8_floor.
    let tool = format!("x{head_pad}{tail_pad}");
    assert!(tool.len() > 2000);
    let tool_len = u32::try_from(tool.len()).expect("fixture fits u32");
    let piece = CompactPiece {
        entry: entry_id(7).expect("nonzero"),
        part: 0,
        off: 0,
        len: tool_len,
        total: tool_len,
        role: Role::Output("tool".into()),
        picture: None,
    };
    let built = items(std::slice::from_ref(&piece), |span| {
        let start = usize::try_from(span.off).expect("span inside piece");
        let end = start + usize::try_from(span.len).expect("span inside piece");
        Ok(tool.as_bytes()[start..end].to_vec())
    })
    .expect("items build");
    let mut cursor = 0usize;
    let mut joined = Vec::new();
    let mut saw_gap = false;
    let mut text_spans = Vec::new();
    let mut gap_omitted: Vec<usize> = Vec::new();
    for item in &built {
        match item {
            Item::Text { span, text, .. } => {
                let start = usize::try_from(span.off).expect("span offset fits");
                let end = start + usize::try_from(span.len).expect("span length fits");
                assert!(tool.is_char_boundary(start));
                assert!(tool.is_char_boundary(end));
                assert!(start >= cursor, "truncated spans stay ordered");
                if start > cursor {
                    saw_gap = true;
                }
                cursor = end;
                text_spans.push((start, end));
                joined.extend_from_slice(text.as_bytes());
            }
            Item::Mark(mark) if mark.starts_with("¶gap:") => {
                saw_gap = true;
                let count = mark
                    .strip_prefix("¶gap:")
                    .and_then(|rest| rest.strip_suffix(" bytes"))
                    .expect("gap mark names the omitted bytes");
                gap_omitted.push(count.parse().expect("gap mark holds a byte count"));
            }
            _ => {}
        }
    }
    assert!(saw_gap, "tool text past 2000 bytes keeps a gap mark");
    // The misaligned head must end on a boundary strictly below 1200,
    // proving utf8_floor stepped back instead of cutting mid-character.
    let (head_start, head_end) = text_spans.first().expect("truncation keeps a head span");
    assert_eq!(*head_start, 0, "the head span starts at zero");
    assert!(
        *head_end < 1200,
        "the misaligned head cut steps back below 1200, got {head_end}"
    );
    assert!(tool.is_char_boundary(*head_end));
    // The gap mark must name exactly the omitted interval between head and tail.
    let (tail_start, _) = text_spans.get(1).expect("truncation keeps a tail span");
    assert_eq!(
        gap_omitted.as_slice(),
        &[tail_start - head_end],
        "the gap mark names the omitted head-to-tail interval"
    );
    assert!(cursor <= tool.len());
    assert!(joined.len() < tool.len(), "truncation omits bytes");
    assert_eq!(joined[0], tool.as_bytes()[0]);

    base64_elision_keeps_contiguity();
}

/// Base64 half of the contiguity contract: a 300-byte run is elided to a
/// gap mark while the surviving spans stay ordered and non-overlapping.
fn base64_elision_keeps_contiguity() {
    let elided = format!("prefix ;base64,{} suffix", "A".repeat(300));
    let elided_len = u32::try_from(elided.len()).expect("fixture fits u32");
    let epiece = CompactPiece {
        entry: entry_id(8).expect("nonzero"),
        part: 0,
        off: 0,
        len: elided_len,
        total: elided_len,
        role: Role::User,
        picture: None,
    };
    let ebuilt = items(std::slice::from_ref(&epiece), |span| {
        let start = usize::try_from(span.off).expect("span inside piece");
        let end = start + usize::try_from(span.len).expect("span inside piece");
        Ok(elided.as_bytes()[start..end].to_vec())
    })
    .expect("items build");
    let mut ecursor = 0usize;
    let mut ejoined = Vec::new();
    let mut egap = false;
    for item in &ebuilt {
        match item {
            Item::Text { span, text, .. } => {
                let start = usize::try_from(span.off).expect("span offset fits");
                let end = start + usize::try_from(span.len).expect("span length fits");
                assert!(start >= ecursor, "elided spans stay ordered");
                ecursor = end;
                ejoined.extend_from_slice(text.as_bytes());
            }
            Item::Mark(mark) if mark.starts_with("¶gap:") => egap = true,
            _ => {}
        }
    }
    assert!(egap, "a 300-byte base64 run is elided to a gap mark");
    assert!(!ejoined.windows(256).any(|w| w.iter().all(|b| b == &b'A')));
}

#[test]
fn draw_renders_deterministic_png_pages() {
    let font = dal_ext::Font::embedded();
    let glyphs = font.glyphs().expect("embedded font parses");
    let grid = Grid {
        cols: 142,
        rows: 49,
        cell_w: 11,
        cell_h: 16,
    };
    let piece = CompactPiece {
        entry: entry_id(4).expect("nonzero"),
        part: 0,
        off: 0,
        len: 11,
        total: 11,
        role: Role::User,
        picture: None,
    };
    let built = items(&[piece], |_| Ok(b"hello world".to_vec())).expect("items build");
    let pages = paginate(glyphs, grid, &built);
    assert_eq!(pages.len(), 1);
    let first = draw(glyphs, grid, &pages[0]).expect("page draws");
    assert_eq!(&first[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    let second = draw(glyphs, grid, &pages[0]).expect("page redraws");
    assert_eq!(first, second);
    let small = Grid {
        cols: 16,
        rows: 1,
        cell_w: 8,
        cell_h: 16,
    };
    let pages = paginate(glyphs, small, &built);
    let kept: usize = pages.iter().map(|page| page.items.len()).sum();
    assert_ne!(pages.len(), 0);
    assert_eq!(kept, built.len());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the round-trip walks every fixture in one table"
)]
fn skill_capture_roundtrip() {
    let description = "歴史漢字の技能説明漢";
    let registry = dal_ext::skills::SkillRegistry::merge(&[(
        "capture-plugin",
        vec![
            dal_ext::skills::RegisteredSkill {
                plugin: "capture-plugin".into(),
                name: "capture-test".into(),
                description: description.into(),
                body: std::sync::Arc::from("body"),
                letter2image: true,
                mcp: None,
            },
            dal_ext::skills::RegisteredSkill {
                plugin: "capture-plugin".into(),
                name: "plain".into(),
                description: "stays text".into(),
                body: std::sync::Arc::from("body"),
                letter2image: false,
                mcp: None,
            },
        ],
    )])
    .expect("registry merges");
    let font = dal_ext::Font::embedded();
    let first_assembly =
        dal_ext::letter::letters(&registry, &font).expect("real renderer assembles");
    assert_eq!(
        first_assembly.chunks.len(),
        1,
        "only the marked skill is captured"
    );
    assert!(
        first_assembly.fallbacks.is_empty(),
        "the CJK ideograph draws"
    );
    let chunk = first_assembly.chunks.first().expect("one admitted chunk");
    assert_eq!(&*chunk.skill, "capture-test");
    assert_eq!(
        &*chunk.source_text, description,
        "source text is byte-equal at capture"
    );
    assert_eq!(&chunk.png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    let digest = blake3::hash(&chunk.png).to_hex().to_string();
    let record = LetterRecord::Skill {
        v: 1,
        id: "capture-test".to_string(),
        png_blob: digest.clone(),
        png_bytes: u32::try_from(chunk.png.len()).expect("png fits u32"),
        width: chunk.width,
        height: chunk.height,
        cell: chunk.cell,
        spans: Vec::new(),
    };
    let body = sonic_rs::to_string(&record).expect("skill record encodes");
    let json = dal_core::RawJson::parse(&body).expect("skill record parses");
    let restored =
        LetterRecord::decode(&json, entry_id(2).expect("nonzero")).expect("skill restores");
    assert_eq!(restored, record);
    assert_eq!(restored.id(), "capture-test");
    let mut blob_store: std::collections::HashMap<[u8; 32], Vec<u8>> =
        std::collections::HashMap::new();
    let png_digest = *blake3::hash(&chunk.png).as_bytes();
    blob_store.insert(png_digest, chunk.png.to_vec());
    let record_store = body.clone();
    drop(first_assembly);
    let reopened_png = blob_store
        .get(&png_digest)
        .expect("blob survives reopen")
        .clone();
    let reopened_json = dal_core::RawJson::parse(&record_store).expect("record survives reopen");
    let reopened_record = LetterRecord::decode(&reopened_json, entry_id(2).expect("nonzero"))
        .expect("record decodes after reopen");
    assert_eq!(reopened_record, record);
    assert_eq!(
        blake3::hash(&reopened_png).to_hex().to_string(),
        digest,
        "reopened blob matches the committed digest"
    );
    let second_assembly = dal_ext::letter::letters(&registry, &font).expect("restart reassembles");
    let redrawn = second_assembly
        .chunks
        .first()
        .expect("restart admits the chunk");
    assert_eq!(
        &*redrawn.source_text, description,
        "source text is byte-equal across restart"
    );
    assert_eq!(
        &*redrawn.png,
        &reopened_png[..],
        "restart keeps the same PNG bytes"
    );
    assert_eq!(
        blake3::hash(&redrawn.png).to_hex().to_string(),
        digest,
        "restart keeps the same PNG digest"
    );
    let source = description;
    let bytes = source.as_bytes().to_vec();
    let len = u32::try_from(bytes.len()).expect("fixture fits");
    let piece = CompactPiece {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 0,
        len,
        total: len,
        role: Role::User,
        picture: None,
    };
    let built = items(std::slice::from_ref(&piece), |span: Span| {
        bytes
            .get(
                usize::try_from(span.off).expect("off fits")
                    ..usize::try_from(span.off + span.len).expect("end fits"),
            )
            .map(<[u8]>::to_vec)
            .ok_or_else(|| SourceError {
                message: "span outside CJK fixture".to_string(),
            })
    })
    .expect("CJK items build");
    assert!(
        built
            .iter()
            .any(|item| matches!(item, Item::Text { text, .. } if text.contains('漢'))),
        "the CJK ideograph survives the history item path"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the error table covers every decline in one list"
)]
fn letter_resolution_errors() {
    assert_eq!(
        RecordError::TooManyLetters { count: 4097 }.to_string(),
        "history record has 4097 letters; the limit is 4096."
    );
    assert_eq!(
        RecordError::TooManySpans { count: 65537 }.to_string(),
        "history record has 65537 spans; the limit is 65536."
    );
    assert_eq!(
        RecordError::BadCell {
            width: 64,
            height: 65
        }
        .to_string(),
        "history record has cell 64x65; each side must be 1 to 64."
    );
    assert_eq!(
        RecordError::BadDigest {
            digest: "XYZ".to_string()
        }
        .to_string(),
        "history record has png_blob \"XYZ\"; the digest must be 64 lowercase hexadecimal digits."
    );
    assert_eq!(
        RecordError::SkillSpans { count: 1 }.to_string(),
        "history record is a skill capture with 1 spans; skill captures carry no spans."
    );
    assert_eq!(
        RecordError::BadSpan {
            entry: 5,
            part: 0,
            off: 0,
            len: 1
        }
        .to_string(),
        "history record has a span [5,0,0,1] that does not name bytes of an earlier entry."
    );
    assert_eq!(
        RecordError::Decode.to_string(),
        "history record is not a version 1 history body."
    );
    assert_eq!(
        HistoryError::Fetch {
            entry: 7,
            message: "gone".to_string()
        }
        .to_string(),
        "cannot read the journal text of entry 7: gone"
    );
    assert_eq!(
        dal_ext::letter::malformed_id_error("x"),
        "letter id is malformed: x"
    );
    assert_eq!(
        dal_ext::letter::missing_id_error("7"),
        "letter 7 does not exist in this session"
    );
    assert_eq!(
        dal_ext::letter::gone_source_error("7"),
        "letter 7 source blob is gone"
    );
    assert_eq!(
        dal_ext::letter::classify_letter_path(""),
        dal_ext::letter::LetterRoute::Empty
    );
    assert_eq!(
        dal_ext::letter::classify_letter_path("7"),
        dal_ext::letter::LetterRoute::Numeric("7".into())
    );
    assert_eq!(
        dal_ext::letter::classify_letter_path("history/1.1"),
        dal_ext::letter::LetterRoute::History
    );
    assert_eq!(
        dal_ext::letter::classify_letter_path("x"),
        dal_ext::letter::LetterRoute::Malformed
    );
    assert_eq!(dal_ext::letter::parse_letter_id("7"), Some(7));
    assert_eq!(dal_ext::letter::parse_letter_id("x"), None);
    let drawn = history_index_line("history/2.3", 4, 7, LetterVisibility::Drawn);
    assert_eq!(drawn, "letter://history/2.3  history image, entries 4-7");
    let shown = history_index_line("history/2.3", 4, 7, LetterVisibility::ShownAsText);
    assert_eq!(
        shown,
        "letter://history/2.3  history image, entries 4-7, shown as text"
    );
    let hidden = history_index_line("history/2.3", 4, 7, LetterVisibility::NotDrawn);
    assert_eq!(
        hidden,
        "letter://history/2.3  history image, entries 4-7, not drawn"
    );
    let session = SessionId::new_v7();
    let items = vec![Item::Text {
        span: Span {
            entry: entry_id(1).expect("nonzero"),
            part: 0,
            off: 0,
            len: 5,
        },
        role: Role::User,
        total: 5,
        text: "hello".into(),
    }];
    let text = letter_text("history/1.1", session, &items);
    assert!(
        text.starts_with(&format!(
            "letter://history/1.1 holds 1 piece of the journal of session {session}."
        )),
        "F-06 names the id, piece count, and session: {text}"
    );
    assert!(
        text.contains("=== entry 1, user, part 0, bytes 0-5 of 5"),
        "{text}"
    );
    assert!(text.ends_with('\n'), "F-06 ends with one final LF");
    assert!(
        !text.ends_with("\n\n"),
        "F-06 has no extra trailing blank line"
    );
}

#[test]
fn letter_text_determinism() {
    let session = SessionId::new_v7();
    let first_span = Span {
        entry: entry_id(1).expect("nonzero"),
        part: 0,
        off: 0,
        len: 5,
    };
    let second_span = Span {
        entry: entry_id(2).expect("nonzero"),
        part: 1,
        off: 2,
        len: 3,
    };
    let items = vec![
        Item::Text {
            span: first_span,
            role: Role::User,
            total: 5,
            text: "hello".into(),
        },
        Item::Text {
            span: second_span,
            role: Role::Assistant,
            total: 9,
            text: "bye".into(),
        },
    ];
    let first = letter_text("history/1.1", session, &items);
    let second = letter_text("history/1.1", session, &items);
    assert_eq!(first, second, "the same inputs give byte-equal letter text");
    assert!(
        first.contains("=== entry 1, user, part 0, bytes 0-5 of 5\nhello"),
        "first span header names entry, role, part, and byte range: {first}"
    );
    assert!(
        first.contains("=== entry 2, assistant, part 1, bytes 2-5 of 9\nbye"),
        "second span header keeps its role words and range: {first}"
    );
    assert!(first.ends_with('\n'));
    assert!(!first.ends_with("\n\n"));
    let rebuilt = vec![
        Item::Text {
            span: first_span,
            role: Role::User,
            total: 5,
            text: "hello".into(),
        },
        Item::Text {
            span: second_span,
            role: Role::Assistant,
            total: 9,
            text: "bye".into(),
        },
    ];
    let reopened = letter_text("history/1.1", session, &rebuilt);
    assert_eq!(
        reopened, first,
        "letter text is byte-equal across a store reopen"
    );
    let single = letter_text(
        "history/1.2",
        session,
        &[Item::Text {
            span: first_span,
            role: Role::Note,
            total: 5,
            text: "hi".into(),
        }],
    );
    assert!(
        single.contains("holds 1 piece of the journal"),
        "one piece uses the singular noun: {single}"
    );
    assert!(
        single.contains("=== entry 1, note, part 0, bytes 0-5 of 5"),
        "{single}"
    );
}

#[test]
fn record_visibility() {
    let services = Arc::new(FakeServices::default());
    services
        .push_raw(
            "letter",
            r#"{"v":1,"id":"history/1.1","kind":"compaction","png_blob":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","png_bytes":1,"width":136,"height":16,"cell":[8,16],"spans":[[1,0,0,1]],"letters":[1]}"#,
        )
        .expect("letter fixture stores");
    services
        .push_raw("note", r#"{"v":1,"text":"unrelated"}"#)
        .expect("other kind stores");
    let cx = ToolCx::for_test(Arc::clone(&services) as Arc<dyn Services>);
    let letters = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
        .block_on(services.records(cx.caller(), "letter"))
        .expect("records read");
    assert_eq!(
        letters.len(),
        1,
        "records(\"letter\") returns only letter bodies"
    );
    let unknown = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds")
        .block_on(services.records(cx.caller(), "unknown-kind-xyz"))
        .expect("unknown kind reads");
    assert!(
        unknown.is_empty(),
        "an unknown kind returns an empty vector"
    );
    let body = letters.first().expect("one letter body");
    let record = LetterRecord::decode(body, entry_id(9).expect("nonzero")).expect("letter decodes");
    assert!(matches!(record, LetterRecord::Compaction { .. }));
    assert_eq!(record.id(), "history/1.1");
    let dream = LetterRecord::Dream {
        v: 1,
        id: "dream/1".to_string(),
        letters: Vec::new(),
        summary: "s".to_string(),
    };
    assert!(
        dream.spans().is_empty(),
        "dream records carry no spans for the image index"
    );
}

#[test]
fn park_policy_lifecycle() {
    let mut state = DreamState::new();
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..5 {
        let actions = transition(&mut state, DreamEvent::JobTransientFailure);
        assert_eq!(actions.len(), 1);
        let DreamAction::Notify(text) = actions.into_iter().next().expect("one notice") else {
            panic!("transient failure before park must notify");
        };
        seen.push(text);
    }
    assert_eq!(seen, vec![failed_notice(); 5]);
    assert_eq!(state.phase, DreamPhase::Idle);
    assert_eq!(state.transient_streak, 5);
    let park = transition(&mut state, DreamEvent::JobTransientFailure);
    assert_eq!(park, vec![DreamAction::Notify(park_notice())]);
    assert_eq!(state.phase, DreamPhase::Parked);
    assert_eq!(state.transient_streak, 6);
    let suppressed = transition(&mut state, DreamEvent::JobTransientFailure);
    assert!(
        suppressed.is_empty(),
        "the F-09 park notice fires exactly once per park"
    );
    let probe = transition(&mut state, DreamEvent::ProbeFailed);
    assert_eq!(
        probe,
        vec![DreamAction::Notify(
            "Auto-dream remains paused; the next probe is in 6 hours.".to_string()
        )]
    );
    assert_eq!(state.phase, DreamPhase::Parked);
    transition(&mut state, DreamEvent::ProbeSucceeded);
    assert_eq!(state.phase, DreamPhase::Idle);
    assert_eq!(state.transient_streak, 0);
    assert_eq!(state.identical_streak, 0);
    let mut again: Vec<String> = Vec::new();
    for _ in 0..5 {
        let actions = transition(&mut state, DreamEvent::JobTransientFailure);
        let DreamAction::Notify(text) = actions.into_iter().next().expect("one notice") else {
            panic!("second-park transient must notify");
        };
        again.push(text);
    }
    assert_eq!(again, vec![failed_notice(); 5]);
    let repark = transition(&mut state, DreamEvent::JobTransientFailure);
    assert_eq!(
        repark,
        vec![DreamAction::Notify(park_notice())],
        "success clears the park so a later park notifies again"
    );
    let mut streak = DreamState::new();
    for _ in 0..2 {
        let actions = transition(
            &mut streak,
            DreamEvent::JobIdenticalFailure("same".to_string()),
        );
        assert_eq!(actions, vec![DreamAction::Notify(failed_notice())]);
    }
    let third = transition(
        &mut streak,
        DreamEvent::JobIdenticalFailure("same".to_string()),
    );
    assert_eq!(third, vec![DreamAction::Notify(park_notice())]);
    let repeat = transition(
        &mut streak,
        DreamEvent::JobIdenticalFailure("same".to_string()),
    );
    assert!(
        repeat.is_empty(),
        "the identical-failure park notice also fires once"
    );
    assert_eq!(consolidated_notice(3), "Auto-dream consolidated 3 letters.");
}
