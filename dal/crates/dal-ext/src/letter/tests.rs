use super::{
    LetterKind, LetterRoute, SkillLetterRecord, classify_letter_path, gone_source_error,
    malformed_id_error, missing_id_error, over_budget_notice, parse_letter_id,
    render_failed_notice, undrawable_notice,
};
use dal_core::{BlobId, RawJson};

fn record(id: &str) -> SkillLetterRecord {
    SkillLetterRecord {
        v: 1,
        id: id.into(),
        kind: LetterKind::Skill,
        skill: "alpha".into(),
        plugin: "demo".into(),
        png_blob: BlobId::from_bytes(b"png"),
        source_blob: BlobId::from_bytes(b"source"),
        png_bytes: 3,
        width: 16,
        height: 32,
        cell: [8, 16],
    }
}

#[test]
fn skill_letter_record_accepts_canonical_ids_and_geometry_limits() {
    let mut last = record("20");
    last.width = 784;
    last.height = 1392;
    assert_eq!(record("1").validated(), Some(1));
    assert_eq!(last.validated(), Some(20));
}

#[test]
fn skill_letter_record_rejects_malformed_ids_and_geometry() {
    for id in ["", "0", "01", "+1", "١"] {
        assert_eq!(record(id).validated(), None);
    }
    assert_eq!(record("21").validated(), None);

    let mut unsupported_version = record("1");
    unsupported_version.v = 2;
    assert_eq!(unsupported_version.validated(), None);

    let mut wrong_cell = record("1");
    wrong_cell.cell = [8, 15];
    assert_eq!(wrong_cell.validated(), None);

    let mut too_wide = record("1");
    too_wide.width = 785;
    assert_eq!(too_wide.validated(), None);

    let mut too_tall = record("1");
    too_tall.height = 1393;
    assert_eq!(too_tall.validated(), None);
}

#[test]
fn skill_letter_record_rejects_unknown_members() {
    let png_blob = BlobId::from_bytes(b"png");
    let source_blob = BlobId::from_bytes(b"source");
    let json = format!(
        r#"{{"v":1,"id":"1","kind":"skill","skill":"alpha","plugin":"demo","png_blob":"{png_blob}","source_blob":"{source_blob}","png_bytes":3,"width":16,"height":32,"cell":[8,16],"extra":true}}"#
    );
    let decoded = RawJson::parse(&json).and_then(|raw| raw.decode_as::<SkillLetterRecord>());
    assert!(
        decoded
            .as_ref()
            .is_err_and(|error| error.to_string().contains("unknown field")),
        "unexpected letter record decode: {decoded:?}"
    );
}

#[test]
fn letter_paths_classify_numeric_history_and_malformed() {
    assert_eq!(classify_letter_path(""), LetterRoute::Empty);
    assert_eq!(classify_letter_path("1"), LetterRoute::Numeric("1".into()));
    assert_eq!(
        classify_letter_path("20"),
        LetterRoute::Numeric("20".into())
    );
    assert_eq!(
        classify_letter_path("21"),
        LetterRoute::Numeric("21".into())
    );
    assert_eq!(parse_letter_id("21"), Some(21));
    assert_eq!(
        classify_letter_path("99999999999"),
        LetterRoute::Numeric("99999999999".into())
    );
    assert_eq!(parse_letter_id("99999999999"), Some(99_999_999_999));
    assert_eq!(
        classify_letter_path("99999999999999999999999"),
        LetterRoute::Numeric("99999999999999999999999".into())
    );
    assert_eq!(parse_letter_id("99999999999999999999999"), None);
    for path in ["0", "01", "+1", "-1", "١", "1.5", "x", "history1"] {
        assert_eq!(classify_letter_path(path), LetterRoute::Malformed);
        assert_eq!(parse_letter_id(path), None);
    }
    for path in ["history/1.1", "dream/2", "history/2.1"] {
        assert_eq!(classify_letter_path(path), LetterRoute::History);
        assert_eq!(parse_letter_id(path), None);
    }
}

#[test]
fn letter_resolver_errors_and_fallback_notices_match_contract() {
    assert_eq!(malformed_id_error("x"), "letter id is malformed: x");
    assert_eq!(malformed_id_error("01"), "letter id is malformed: 01");
    assert_eq!(
        missing_id_error("3"),
        "letter 3 does not exist in this session"
    );
    assert_eq!(
        missing_id_error("21"),
        "letter 21 does not exist in this session"
    );
    assert_eq!(gone_source_error("1"), "letter 1 source blob is gone");
    assert_eq!(
        undrawable_notice("alpha", "demo", 0x1F600),
        "letter 'alpha' from plugin 'demo' fell back to text: first undrawable codepoint U+1F600."
    );
    assert_eq!(
        over_budget_notice("alpha", "demo"),
        "letter 'alpha' from plugin 'demo' fell back to text: image byte budget exceeded."
    );
    assert_eq!(
        render_failed_notice("alpha", "demo"),
        "letter 'alpha' from plugin 'demo' fell back to text: render failed."
    );
}

#[test]
fn extension_registers_letter_scheme() {
    let built = super::extension().expect("letter extension builds");
    assert!(built.prompt_section().is_none());
    assert_eq!(built.schemes().len(), 1);
    assert_eq!(&*built.schemes()[0].0, "letter");
}
