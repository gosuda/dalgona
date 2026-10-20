use super::*;

/// Names that sit one edit away from a built-in: a prefix match, a suffix
/// match, a substring match, and padding. None of them is a built-in.
const NEAR_MISSES: [&str; 12] = [
    "computer_use",
    "my_computer",
    "web_search_v2",
    "xcode_execution",
    "text_editor2",
    "web-search",
    "computer ",
    " computer",
    "text_edito",
    "code",
    "search",
    "_computer",
];

#[test]
fn only_an_exact_builtin_name_travels_unprefixed() {
    for name in NEAR_MISSES {
        assert_eq!(
            encode_tool_name(name),
            format!("_{name}"),
            "{name:?} is not a built-in and must carry the prefix"
        );
    }
    for name in [
        "web_search",
        "WEB_SEARCH",
        "Web_Search",
        "code_execution",
        "Code_Execution",
        "text_editor",
        "TEXT_EDITOR",
        "computer",
        "Computer",
    ] {
        assert_eq!(
            encode_tool_name(name),
            name,
            "{name:?} is a built-in under any ASCII case and keeps its spelling"
        );
    }
}

#[test]
fn every_name_survives_the_wire_round_trip() {
    for name in [
        "",
        "_",
        "__",
        "___x",
        "read",
        "_read",
        "Read",
        "web_search",
        "Web_Search",
        "_computer",
        "__computer",
        "é",
        "ツール",
    ]
    .into_iter()
    .chain(NEAR_MISSES)
    {
        let wire = encode_tool_name(name);
        assert_eq!(
            decode_tool_name(&wire),
            name,
            "{name:?} went out as {wire:?} and must come back unchanged"
        );
    }
}

#[test]
fn distinct_names_never_share_a_wire_spelling() {
    let names = [
        "computer",
        "_computer",
        "__computer",
        "web_search",
        "_web_search",
        "read",
        "_read",
        "__read",
        "READ",
        "Read",
    ];
    let wires: Vec<String> = names
        .iter()
        .map(|name| encode_tool_name(name).into_owned())
        .collect();
    for (first, a) in names.iter().zip(&wires).enumerate() {
        for (second, b) in names.iter().zip(&wires).enumerate() {
            if first != second {
                assert_ne!(
                    a.1, b.1,
                    "{:?} and {:?} would answer to one wire name",
                    a.0, b.0
                );
            }
        }
    }
}

#[test]
fn decode_removes_exactly_one_prefix() {
    assert_eq!(decode_tool_name("__x"), "_x");
    assert_eq!(decode_tool_name("___"), "__");
    assert_eq!(decode_tool_name("_"), "");
    assert_eq!(decode_tool_name(""), "");
    assert_eq!(decode_tool_name("web_search"), "web_search");
    assert_eq!(
        decode_tool_name("x_"),
        "x_",
        "only a leading prefix is removed"
    );
}

#[test]
fn user_agent_carries_whatever_version_it_is_given() {
    assert_eq!(user_agent("9.9.9"), "claude-cli/9.9.9 (external, cli)");
    assert_eq!(user_agent(""), "claude-cli/ (external, cli)");
}

/// The pin is raised by hand at the source; a typo (`v2.1.281`, `2.1`,
/// `2.1.28O`) would put a malformed `user-agent` on every OAuth request.
#[test]
fn the_pinned_version_is_three_numeric_components() {
    let parts: Vec<&str> = CLAUDE_CODE_VERSION.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "{CLAUDE_CODE_VERSION:?} is not major.minor.patch"
    );
    for part in parts {
        assert!(
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            "{CLAUDE_CODE_VERSION:?} has a non-numeric component {part:?}"
        );
    }
}
