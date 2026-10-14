//! Contract tests for the tolerant codec: each malformation accepted
//! must both recover and leave a repair record, and every class of
//! malformation the grammar refuses must fail in the same way strict
//! parsing would have failed.

use proptest::prop_assert;
use serde::Deserialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::*;

fn value(src: &str) -> sonic_rs::Value {
    parse(src).unwrap_or_else(|err| panic!("{src:?} must parse: {err}"))
}

#[test]
fn strict_json_parses_and_is_not_repaired() {
    let src = r#"{"a": 1, "b": [true, null, 1.5, "x"], "c": {"d": "y"}}"#;
    let v = value(src);
    assert_eq!(v["a"].as_u64(), Some(1));
    assert_eq!(v["b"][2].as_f64(), Some(1.5));
    assert_eq!(v["c"]["d"].as_str(), Some("y"));

    let (_raw, repairs) = canonicalize(src).expect("strict input canonicalizes");
    assert!(repairs.is_empty(), "strict JSON must not record repairs");
}

#[test]
fn tolerant_surface_recovers_each_malformation() {
    let cases = [
        // Single-quoted strings and keys.
        (r"{'a': 'b'}", 2),
        // Unquoted keys.
        (r"{a: 1}", 1),
        // Line, block, and doc comments.
        ("{ // c\n\"a\": /* c */ 1 /** c */}", 3),
        // Python literals.
        (r#"{"t": True, "f": False, "n": None}"#, 3),
        // Rust/JS radix integers.
        (r#"{"h": 0xff, "b": 0b101, "o": 0o17}"#, 3),
        // Leading dot, trailing dot, leading plus, leading zeros.
        (r#"{"a": .5, "b": 1., "c": +1, "d": 007}"#, 4),
        // Trailing and doubled commas.
        (r#"{"a": 1,}"#, 1),
        (r"[1,,2]", 1),
        // Raw control characters inside strings.
        ("{\"a\": \"x\ty\n\"}", 2),
        // Invalid escape is preserved as a literal.
        (r#"{"a": "x\qy"}"#, 1),
    ];
    for (src, min_repairs) in cases {
        let (raw, repairs) =
            canonicalize(src).unwrap_or_else(|| panic!("{src:?} must canonicalize"));
        assert!(
            repairs.len() >= min_repairs,
            "{src:?} recorded {} repairs",
            repairs.len()
        );
        // Canonical output is byte-valid strict JSON that re-parses to the
        // same value.
        let reparsed = parse(raw.as_str()).expect("canonical output must re-parse");
        let repaired = parse(src).expect("tolerant parse must accept");
        assert_eq!(reparsed, repaired, "{src:?} canonical ≠ tolerant value");
    }
}

#[test]
fn comma_tolerance_preserves_element_values() {
    let v = value(r"[1,,2,]");
    assert_eq!(v[0].as_u64(), Some(1));
    assert_eq!(v[1].as_u64(), Some(2));
    let (_raw, repairs) = canonicalize(r"[1,,2,]").unwrap();
    let kinds: Vec<_> = repairs.as_slice().iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![RepairKind::StrayComma, RepairKind::TrailingComma]
    );
}

#[test]
fn inner_quote_marks_word_boundary_but_never_swallows() {
    // Sloppy inner quote: boundary honored, rest of the line is kept.
    let v = value(r#"{"desc": "He said "hi" and left", "n": 1}"#);
    let desc = v["desc"].as_str().expect("desc is a string");
    assert!(desc.contains("hi"), "{desc:?} lost the inner quote content");
    assert!(desc.contains("and left"), "{desc:?} lost the tail");
    assert_eq!(v["n"].as_u64(), Some(1));
    let (_raw, repairs) = canonicalize(r#"{"desc": "He said "hi" and left", "n": 1}"#).unwrap();
    assert!(
        repairs
            .as_slice()
            .iter()
            .any(|r| r.kind == RepairKind::InnerQuote)
    );

    // Single-quoted strings get the same recovery.
    let v = value("{'desc': 'it isn't here', 'n': 2}");
    assert!(v["desc"].as_str().unwrap().contains("isn"));
    assert_eq!(v["n"].as_u64(), Some(2));
}

#[test]
fn unterminated_string_fails_closed() {
    assert!(matches!(
        parse("\"abc"),
        Err(ParseError::UnterminatedString)
    ));
    assert!(matches!(
        parse("{'a': 'b"),
        Err(ParseError::UnterminatedString)
    ));
    assert!(canonicalize("\"abc").is_none());
}

#[test]
fn number_discipline() {
    assert_eq!(value("0").as_u64(), Some(0));
    assert_eq!(value("-7").as_i64(), Some(-7));
    assert_eq!(value("1e3").as_f64(), Some(1000.0));
    assert_eq!(value("0xff").as_u64(), Some(255));
    // Large u64 stays exact.
    assert_eq!(value("18446744073709551615").as_u64(), Some(u64::MAX));
    // Beyond u64 spills to i64/f64 without failing.
    assert!(value("18446744073709551616").as_f64().is_some());
    // Non-finite literals are refused, not coerced.
    for bad in ["NaN", "Infinity", "-Infinity", "+Infinity", "inf"] {
        assert!(parse(bad).is_err(), "{bad} must not parse as a number");
    }
    // A malformed radix is refused, not truncated to 0.
    assert!(parse("0xzz").is_err());
    assert!(parse("0b2").is_err());
}

#[test]
fn bareword_values_and_their_guards() {
    // Bareword accepted only as a container member value.
    let v = value("{a: bare}");
    assert_eq!(v["a"].as_str(), Some("bare"));
    // Delimiters terminate it.
    let v = value("{a: git://x, b: 1}");
    assert_eq!(v["a"].as_str(), Some("git://x"));
    assert_eq!(v["b"].as_u64(), Some(1));
    // Key-like `:` inside a bareword makes the member malformed.
    assert!(parse("{a: http:x}").is_err());
    assert!(parse("{a: foo:1}").is_err());
    // Reserved words never fall through to barewords.
    assert!(parse("{a: undefined}").is_err());
    // Barewords are not a top-level escape hatch.
    assert!(parse("bare").is_err());
}

#[test]
fn structural_failures_match_strict_intent() {
    // Missing separators and trailing garbage fail, they do not guess.
    assert!(matches!(
        parse("{} {}"),
        Err(ParseError::TrailingCharacters(_))
    ));
    assert!(matches!(
        parse("[1 2]"),
        Err(ParseError::ExpectedCommaOrBracket(_))
    ));
    // `{"a" 1}` has no recoverable reading: a missing colon makes the
    // inner-quote recovery consume the rest — it fails either way.
    assert!(parse("{\"a\" 1}").is_err());
    assert!(matches!(
        parse("{\"a\":"),
        Err(ParseError::ExpectedValue(_))
    ));
    assert!(matches!(parse(""), Err(ParseError::UnexpectedEnd)));
    // UTF-8 errors come back with the offset.
    assert!(matches!(
        from_slice::<&str>(b"\"\xff\""),
        Err(ParseError::InvalidUtf8(1))
    ));
}

#[test]
fn depth_is_bounded() {
    let deep = "[".repeat(200) + &"]".repeat(200);
    assert!(matches!(parse(&deep), Err(ParseError::DepthExceeded)));
    // parse_partial auto-closes rather than recursing forever — it
    // must not stack-overflow either.
    let _ = parse_partial(&"[".repeat(10_000));
}

#[test]
fn deserializer_drives_typed_targets() {
    #[derive(Deserialize, Debug, PartialEq)]
    enum E {
        A,
        B(i64),
    }
    #[derive(Deserialize, Debug, PartialEq)]
    struct Args {
        cmd: String,
        #[serde(default)]
        verbose: bool,
        flags: Option<Vec<i64>>,
    }
    let args: Args = from_str("{cmd: 'ls', verbose: True, flags: [0x1, -2]}")
        .expect("typed decode off the tolerant grammar");
    assert_eq!(args.cmd, "ls");
    assert!(args.verbose);
    assert_eq!(args.flags, Some(vec![1, -2]));

    // Borrowed strings reach the target without copying.
    let s: &str = from_str::<&str>("'raw'").expect("borrowed str");
    assert_eq!(s, "raw");

    // Enum: unit variant via bareword-like keyword is not a thing —
    // enums decode through the standard map/index forms.
    let e: E = from_str(r#""A""#).expect("unit variant");
    assert_eq!(e, E::A);
    let e: E = from_str(r#"{"B": 4}"#).expect("newtype variant");
    assert_eq!(e, E::B(4));

    // A type mismatch surfaces serde's own error.
    let err = from_str::<u32>("'x'").unwrap_err();
    assert!(err.to_string().contains("invalid type"), "{err}");
}

#[test]
fn classify_marks_complete_prefix_and_dead_ends() {
    assert_eq!(classify(r#"{"a": [1, 2, 3]}"#), Prefix::Complete);
    assert_eq!(classify(r#" {"a": [1, 2, 3]} "#), Prefix::Complete);
    assert_eq!(classify(r#"{"a": [1,"#), Prefix::Prefix);
    assert_eq!(classify(r#"{"a": "x"#), Prefix::Prefix);
    assert_eq!(classify(r#"{"a": tru"#), Prefix::Prefix);
    assert_eq!(classify(""), Prefix::Prefix);
    // Dead ends: second top-level value, raw control char, bad token.
    assert_eq!(classify("{} {}"), Prefix::Invalid);
    assert_eq!(classify("{\"a\": \"x\ty\"}"), Prefix::Invalid);
    assert_eq!(classify("{a:1}"), Prefix::Invalid); // strict only
    assert_eq!(classify("1..2"), Prefix::Invalid);
    // A top-level number at end of input is complete by shape even
    // though it could extend — the documented ambiguity.
    assert_eq!(classify("12"), Prefix::Complete);
}

#[test]
fn parse_partial_autocloses_and_rolls_back() {
    // Empty and junk input yield an empty object, never a panic.
    assert!(
        parse_partial("")
            .as_object()
            .is_none_or(sonic_rs::Object::is_empty)
    );
    assert!(
        parse_partial("   ")
            .as_object()
            .is_none_or(sonic_rs::Object::is_empty)
    );

    // Truncated containers close at the last complete member.
    let v = parse_partial(r#"{"a": 1, "b": {"c": [1, 2"#);
    assert_eq!(v["a"].as_u64(), Some(1));
    // The trailing `2` is an incomplete atom (it could still extend), so
    // the array keeps only `1` — the same rollback as for keywords.
    assert_eq!(v["b"]["c"][0].as_u64(), Some(1));
    assert!(
        v["b"]["c"][1].is_null(),
        "incomplete element must roll back: {v:?}"
    );

    // An incomplete trailing atom rolls back to the member before it:
    // the half-streamed keyword is NOT committed as a value.
    let v = parse_partial(r#"{"a": 1, "b": tr"#);
    assert_eq!(v["a"].as_u64(), Some(1));
    assert!(v["b"].is_null(), "incomplete keyword must roll back: {v:?}");

    // Same rollback for a half-written number or bareword.
    let v = parse_partial(r#"{"a": 1, "b": 12"#);
    assert!(v["b"].is_null(), "incomplete number must roll back: {v:?}");

    // An open string still yields its decoded-so-far content.
    let v = parse_partial(r#"{"a": "hel"#);
    assert_eq!(v["a"].as_str(), Some("hel"));
}

#[test]
fn pull_reads_paths_inside_unfinished_documents() {
    let src = r#"{"args": {"cmd": "ls", "flags": ["-l", "-a"]}, "id": 7}"#;

    // Complete pull of a scalar and a container.
    let pulled = pull(
        src,
        &[PullSegment::Key("args"), PullSegment::Key("cmd")],
        PullMode::Complete,
        true,
    )
    .expect("pull must answer")
    .expect("value is ready");
    assert_eq!(
        pulled.kind,
        PulledKind::Complete(sonic_rs::Value::from("ls"))
    );
    assert_eq!(pulled.matched_key.as_deref(), Some("cmd"));

    let pulled = pull(
        src,
        &[
            PullSegment::Key("args"),
            PullSegment::Key("flags"),
            PullSegment::Index(1),
        ],
        PullMode::Complete,
        true,
    )
    .expect("pull must answer")
    .expect("value is ready");
    assert_eq!(
        pulled.kind,
        PulledKind::Complete(sonic_rs::Value::from("-a"))
    );

    // Alias segment resolves whichever spelling occurs first.
    let pulled = pull(
        src,
        &[PullSegment::Keys(&["missing", "id"])],
        PullMode::Complete,
        true,
    )
    .expect("pull must answer")
    .expect("value is ready");
    assert_eq!(pulled.kind, PulledKind::Complete(sonic_rs::Value::from(7)));
    assert_eq!(pulled.matched_key.as_deref(), Some("id"));

    // Started pull resolves on the first token.
    let pulled = pull(src, &[PullSegment::Key("args")], PullMode::Started, false)
        .expect("pull must answer")
        .expect("started value is ready");
    assert_eq!(pulled.kind, PulledKind::Started(PulledValueKind::Object));
}

#[test]
fn pull_pendings_misses_and_mismatches() {
    // Open document: container pull on a cut member pends, not fails.
    let cut = r#"{"a": {"b": 1"#;
    assert_eq!(
        pull(cut, &[PullSegment::Key("a")], PullMode::Complete, false).unwrap(),
        None,
        "unfinished container must pend"
    );
    // Same cut on a final input is an Incomplete error.
    let err = pull(cut, &[PullSegment::Key("a")], PullMode::Complete, true).unwrap_err();
    assert_eq!(err.kind, PullIssueKind::Incomplete);

    // Missing key on a closed container.
    let err = pull(
        r#"{"a": 1}"#,
        &[PullSegment::Key("b")],
        PullMode::Complete,
        true,
    )
    .unwrap_err();
    assert_eq!(err.kind, PullIssueKind::Missing);

    // Wrong-shape segment reports the observed type.
    let err = pull("[1]", &[PullSegment::Key("a")], PullMode::Complete, true).unwrap_err();
    assert_eq!(err.kind, PullIssueKind::TypeMismatch { found: "array" });

    // First-wins: a duplicated key resolves the FIRST occurrence, which
    // is what the incremental reader saw, not the DOM's last write.
    let pulled = pull(
        r#"{"a": 1, "a": 2}"#,
        &[PullSegment::Key("a")],
        PullMode::Complete,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(pulled.kind, PulledKind::Complete(sonic_rs::Value::from(1)));
}

#[test]
fn pull_streams_string_chunks_and_lines() {
    // Chunk pulls emit only stable decoded bytes after the offset.
    let src = r#"{"t": "hello wo"#;
    let pulled = pull(src, &[PullSegment::Key("t")], PullMode::Chunk(0), false)
        .expect("pull must answer")
        .expect("open string has stable bytes");
    assert_eq!(
        pulled.kind,
        PulledKind::Chunk {
            value: "hello wo".into(),
            complete: false
        }
    );

    // Line pulls only emit up to the last newline while open.
    let src = r#"{"t": "one\ntwo\nthr"#;
    let pulled = pull(src, &[PullSegment::Key("t")], PullMode::Line(0), false)
        .expect("pull must answer")
        .expect("open string has complete lines");
    assert_eq!(
        pulled.kind,
        PulledKind::Chunk {
            value: "one\ntwo\n".into(),
            complete: false
        }
    );

    // Once the string closes, the final unterminated line flushes.
    let src = r#"{"t": "one\ntwo"}"#;
    let pulled = pull(src, &[PullSegment::Key("t")], PullMode::Line(0), true)
        .expect("pull must answer")
        .expect("closed string flushes");
    assert_eq!(
        pulled.kind,
        PulledKind::Chunk {
            value: "one\ntwo".into(),
            complete: true
        }
    );

    // Chunk pulls on non-strings report the observed type.
    let err = pull(
        r#"{"n": 1}"#,
        &[PullSegment::Key("n")],
        PullMode::Chunk(0),
        true,
    )
    .unwrap_err();
    assert_eq!(err.kind, PullIssueKind::TypeMismatch { found: "number" });
}

#[test]
fn repair_evidence_carries_the_source_path() {
    let (_raw, repairs) = canonicalize("{a: {'b': [True,]}, c: .5}").unwrap();
    let by_kind = |kind| {
        repairs
            .as_slice()
            .iter()
            .find(|r| r.kind == kind)
            .map(|r| r.path.clone())
    };
    assert_eq!(
        by_kind(RepairKind::PythonLiteral),
        Some(vec![
            Segment::Key("a".into()),
            Segment::Key("b".into()),
            Segment::Index(0)
        ])
    );
    assert_eq!(
        by_kind(RepairKind::RelaxedNumber),
        Some(vec![Segment::Key("c".into())])
    );
    assert_eq!(
        by_kind(RepairKind::TrailingComma),
        Some(vec![Segment::Key("a".into()), Segment::Key("b".into())])
    );
}

proptest::proptest! {
    /// Every public entry point must stay panic-free on arbitrary input;
    /// any canonical output must re-parse under the strict codec.
    #[test]
    fn entry_points_never_panic_and_canonical_output_is_strict(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256),
        ended in proptest::prelude::any::<bool>(),
    ) {
        let _ = from_slice::<sonic_rs::Value>(&bytes);
        if let Ok(src) = std::str::from_utf8(&bytes) {
            let _ = parse(src);
            let _ = parse_partial(src);
            let _ = classify(src);
            let path = [
                PullSegment::Key("a"),
                PullSegment::Key("b"),
                PullSegment::Index(2),
            ];
            for mode in [
                PullMode::Started,
                PullMode::Complete,
                PullMode::Chunk(4),
                PullMode::Line(4),
            ] {
                let _ = pull(src, &path, mode, ended);
            }
            if let Some((raw, _repairs)) = canonicalize(src) {
                prop_assert!(
                    sonic_rs::from_slice::<sonic_rs::Value>(raw.as_str().as_bytes()).is_ok(),
                    "canonical output must re-parse strictly: {}",
                    raw.as_str()
                );
            }
        }
    }

    /// Structurally plausible documents (generated, not arbitrary bytes)
    /// exercise deeper paths: the same panic-freedom and strict-output
    /// contract holds.
    #[test]
    fn plausible_documents_stay_canonical(
        src in "(\\{\"?([a-z]{0,4}\")?:?([0-9xob.+eE-]{0,6}|'(ab|cd)*'|true|false|null|nan|NaN)( *, *|; *)\\}?){1,8}",
    ) {
        let _ = parse(&src);
        let _ = parse_partial(&src);
        if let Some((raw, _repairs)) = canonicalize(&src) {
            prop_assert!(
                sonic_rs::from_slice::<sonic_rs::Value>(raw.as_str().as_bytes()).is_ok(),
                "canonical output must re-parse strictly: {}",
                raw.as_str()
            );
        }
    }
}
