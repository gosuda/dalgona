use proptest::prelude::*;

use super::{LexError, tokens};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn p04_lexer_table() -> TestResult {
    let cases: &[(&str, &[&str])] = &[
        ("", &[]),
        (" \t ", &[]),
        ("a b", &["a", "b"]),
        ("a\t\tb ", &["a", "b"]),
        ("'a b' c", &["a b", "c"]),
        ("\"a b\" c", &["a b", "c"]),
        (r"a\ b", &["a b"]),
        (r"'a\b'", &[r"a\b"]),
        (r#""a\"b""#, &["a\"b"]),
        (r#""a\\b""#, &[r"a\b"]),
        (r"\'x", &["'x"]),
        ("x'y z'w", &["xy zw"]),
        ("''", &[""]),
        ("\"\" a", &["", "a"]),
        ("a\nb", &["a\nb"]),
        ("a\u{a0}b", &["a\u{a0}b"]),
        ("héllo 'wörld'", &["héllo", "wörld"]),
        ("--f=v -- x", &["--f=v", "--", "x"]),
        ("$HOME *.rs `x`", &["$HOME", "*.rs", "`x`"]),
    ];
    for (raw, expected) in cases {
        let got = tokens(raw)?;
        let got = got.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
        assert_eq!(&got, expected, "input {raw:?}");
    }
    Ok(())
}

#[test]
fn p04_lexer_rejects_unclosed_quotes_and_escapes() {
    assert_eq!(
        tokens("a 'b"),
        Err(LexError::UnclosedQuote { quote: '\'', at: 2 })
    );
    assert_eq!(
        tokens("\"b"),
        Err(LexError::UnclosedQuote { quote: '"', at: 0 })
    );
    assert_eq!(
        tokens(r#""b\"#),
        Err(LexError::UnclosedQuote { quote: '"', at: 0 })
    );
    assert_eq!(tokens(r"ab\"), Err(LexError::TrailingEscape { at: 2 }));
    assert_eq!(
        tokens(r"'a\'b'"),
        Err(LexError::UnclosedQuote { quote: '\'', at: 5 })
    );
}

#[derive(Clone, Copy, Debug)]
enum Style {
    Escaped,
    Single,
    Double,
}

fn encode(token: &str, style: Style) -> String {
    match style {
        Style::Single if !token.contains('\'') => format!("'{token}'"),
        Style::Double => {
            let mut out = String::from("\"");
            for ch in token.chars() {
                if matches!(ch, '"' | '\\') {
                    out.push('\\');
                }
                out.push(ch);
            }
            out.push('"');
            out
        }
        Style::Single | Style::Escaped if token.is_empty() => String::from("''"),
        Style::Single | Style::Escaped => {
            let mut out = String::new();
            for ch in token.chars() {
                if matches!(ch, ' ' | '\t' | '\'' | '"' | '\\') {
                    out.push('\\');
                }
                out.push(ch);
            }
            out
        }
    }
}

fn style() -> impl Strategy<Value = Style> {
    prop_oneof![
        Just(Style::Escaped),
        Just(Style::Single),
        Just(Style::Double)
    ]
}

fn piece() -> impl Strategy<Value = (String, Style)> {
    (
        proptest::collection::vec(
            prop_oneof![
                Just(' '),
                Just('\t'),
                Just('\''),
                Just('"'),
                Just('\\'),
                Just('\n'),
                Just('é'),
                proptest::char::range('a', 'z'),
            ],
            0..6,
        )
        .prop_map(|chars| chars.into_iter().collect::<String>()),
        style(),
    )
}

fn separator() -> impl Strategy<Value = String> {
    proptest::collection::vec(prop_oneof![Just(' '), Just('\t')], 1..3)
        .prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #[test]
    fn p04_lexer_inverts_an_independent_encoder(
        tokens_in in proptest::collection::vec(proptest::collection::vec(piece(), 1..3), 0..5),
        seps in proptest::collection::vec(separator(), 6),
        lead in proptest::collection::vec(prop_oneof![Just(' '), Just('\t')], 0..2),
    ) {
        let mut raw = lead.into_iter().collect::<String>();
        let mut expected = Vec::new();
        for (index, pieces) in tokens_in.iter().enumerate() {
            if index > 0 {
                raw.push_str(&seps[index]);
            }
            let mut token = String::new();
            for (text, style) in pieces {
                raw.push_str(&encode(text, *style));
                token.push_str(text);
            }
            expected.push(token);
        }
        raw.push_str(&seps[0]);
        let got = tokens(&raw).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let got = got.iter().map(AsRef::as_ref).collect::<Vec<&str>>();
        prop_assert_eq!(got, expected.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn p04_lexer_never_splits_on_non_separator_text(text in "[^ \t'\"\\\\]{1,24}") {
        let got = tokens(&text).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(got, vec![Box::<str>::from(text.as_str())]);
    }
}
