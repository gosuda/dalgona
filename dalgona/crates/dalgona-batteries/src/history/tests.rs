// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History battery behavior tests.

use std::num::NonZeroU64;

use dal_core::EntryId;

use super::config::{HistoryConfig, HistoryConfigError, parse_config};
use super::draw::{Grid, draw, paginate};
use super::dream::{
    DreamEvent, DreamFileError, DreamPhase, DreamState, check_dream_file, decode_dream_file,
    transition,
};
use super::records::{LetterRecord, RecordError};
use super::selection::{
    LetterVisibility, history_index_line, select_oldest_plus_newest,
    select_oldest_plus_newest_by_bytes,
};
use super::spans::{
    CompactPiece, HistoryError, Item, Role, SourceError, Span, items, retained_segments,
};

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
    assert!(marks[0].is_some());
    assert!(marks[1].is_some());
    assert!(marks[2].is_some());
    assert!(marks[3].is_some());
    assert!(marks[4].is_some());
    assert!(marks[5].is_some());
    assert!(marks[6].is_none());
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
