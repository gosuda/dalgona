// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Behavior tests for the bundled skills extension.

use std::collections::BTreeSet;
use std::error::Error;

use dal_core::{Name, Origin};
use dal_ext::ttsr::{Origin as RuleOrigin, rulefile::parse_rulefile};
use dalgona_batteries::skills::{SKILLS_DOC, skills};

#[test]
fn test_discipline_rule_frontmatter_is_shipped_contract() -> Result<(), Box<dyn Error>> {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.dalgona/rules/test-discipline.md"
    ));
    let (rule, notes) = parse_rulefile(
        "test-discipline",
        RuleOrigin::User(std::path::PathBuf::from("dalgona/.dalgona/rules")),
        source.as_bytes(),
    )
    .map_err(|problems| format!("rule frontmatter rejected: {problems:?}"))?;
    assert!(notes.is_empty(), "unexpected parser notes: {notes:?}");
    assert_eq!(
        rule.description.as_deref(),
        Some("Test discipline for Rust source, tests, and benchmarks.")
    );
    let globs = rule
        .globs
        .as_ref()
        .ok_or_else(|| std::io::Error::other("rule has no compiled globs"))?;
    assert_eq!(globs.len(), 1, "the shipped rule has one Rust glob");
    assert!(globs.is_match("src/lib.rs"));
    assert!(globs.is_match("tests/quality.rs"));
    assert!(!globs.is_match("notes.md"));
    Ok(())
}

#[test]
fn bundled_extension_exposes_all_shipped_skill_names() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    assert_eq!(extension.origin(), Origin::Bundled);

    let mut names: Vec<_> = extension
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "delegate-with-contracts",
            "find-anything",
            "initializer-and-sprints"
        ]
    );
    Ok(())
}

#[test]
fn extension_carrier_is_bundled_data_only() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    assert_eq!(extension.name(), "skills", "carrier keeps its fixed name");
    assert_eq!(extension.origin(), Origin::Bundled);
    assert!(
        extension.inject().is_empty(),
        "a skill pack declares no service"
    );
    assert!(
        extension.tools().is_empty(),
        "skills carry records, never tools"
    );
    assert!(
        extension.commands().is_empty(),
        "skills carry records, never commands"
    );
    assert_eq!(
        extension.version(),
        env!("CARGO_PKG_VERSION"),
        "carrier tracks the package version"
    );
    assert_eq!(extension.skills().len(), 3, "exactly three bundled skills");
    for name in [
        "find-anything",
        "delegate-with-contracts",
        "initializer-and-sprints",
    ] {
        assert!(
            SKILLS_DOC.contains(name),
            "manual page names every shipped skill"
        );
    }
    assert!(
        SKILLS_DOC.contains("no second skill resolver"),
        "manual page pins single-resolver discovery"
    );
    Ok(())
}

#[test]
fn skill_metadata_respects_battery_bounds() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    let records = extension.skills();
    let mut seen_names = BTreeSet::new();
    let mut seen_bodies = BTreeSet::new();
    for skill in records {
        let name = skill.name.as_str();
        assert!(seen_names.insert(name), "duplicate skill name: {name}");
        // The fixed names satisfy the registry law; a parse failure here
        // means the battery stopped being loadable.
        Name::parse(name)?;
        assert!(!skill.description.is_empty(), "{name} needs a description");
        assert!(
            skill.description.chars().count() <= 300,
            "{name} description exceeds 300 chars"
        );
        assert!(!skill.letter2image, "{name} must stay text-only");
        assert!(skill.mcp.is_none(), "{name} must declare no MCP servers");
        let body: &str = &skill.body;
        assert!(!body.is_empty(), "{name} body is empty");
        assert!(!body.starts_with('\u{feff}'), "{name} body carries a BOM");
        assert!(!body.contains('\r'), "{name} body uses CRLF");
        assert!(
            body.ends_with('\n') && !body.ends_with("\n\n"),
            "{name} body must end with one LF"
        );
        assert!(body.lines().count() <= 200, "{name} body exceeds 200 lines");
        assert!(
            body.len() <= 131_072,
            "{name} body exceeds the registry byte ceiling"
        );
        for forbidden in ["dal.tool", "dal.on", "ctx.", "credential"] {
            assert!(
                !body.contains(forbidden),
                "{name} body leaks capability text {forbidden}"
            );
        }
        assert!(
            seen_bodies.insert(body),
            "{name} body duplicates another skill"
        );
    }
    assert_eq!(seen_names.len(), 3);
    Ok(())
}

#[test]
fn find_anything_covers_search_surfaces_deterministically() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    let body: &str = &extension
        .skills()
        .iter()
        .find(|skill| skill.name.as_str() == "find-anything")
        .ok_or_else(|| std::io::Error::other("find-anything is missing"))?
        .body;
    let headings = [
        "## find",
        "## grep",
        "## symbol",
        "## monitor",
        "## web_search",
        "## Deferred tools",
    ];
    let mut cursor = 0;
    for heading in headings {
        let at = body[cursor..]
            .find(heading)
            .ok_or_else(|| std::io::Error::other(format!("missing heading {heading}")))?;
        cursor += at + heading.len();
    }
    for example in [
        "search(mode = \"find\"",
        "search(mode = \"grep\"",
        "search(mode = \"symbol\"",
        "monitor(action = \"watch\"",
        "web_search(query =",
        "tool_search(query =",
    ] {
        assert!(
            body.contains(example),
            "find-anything omits example {example}"
        );
    }
    for tool in ["search", "monitor", "web_search", "tool_search", "procs"] {
        assert!(body.contains(tool), "find-anything names no {tool} surface");
    }
    // Two builds are byte-identical: discovery order never depends on
    // declaration order or build timing.
    let again = skills()?;
    assert_eq!(
        extension.skills(),
        again.skills(),
        "rebuilt records must be byte-identical"
    );
    let mut names: Vec<&str> = extension
        .skills()
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "delegate-with-contracts",
            "find-anything",
            "initializer-and-sprints"
        ],
        "sorted names must be bytewise deterministic"
    );
    Ok(())
}

#[test]
fn find_anything_promotion_path_is_side_effect_free() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    let body: &str = &extension
        .skills()
        .iter()
        .find(|skill| skill.name.as_str() == "find-anything")
        .ok_or_else(|| std::io::Error::other("find-anything is missing"))?
        .body;
    assert!(
        body.contains("offered in the active tool set"),
        "promotion must stay conditional on what is offered"
    );
    assert!(
        body.contains("If `tool_search` is offered"),
        "promotion must not promise an unconditional tool_search"
    );
    assert!(
        body.contains("lists matches without activating them"),
        "search must be pinned side-effect-free"
    );
    assert!(
        body.contains("Calling a listed tool activates it"),
        "promotion must be pinned to the by-name call"
    );
    assert!(
        body.contains("When Anthropic native tool search is offered instead"),
        "native discovery alternative must be named"
    );
    assert!(
        body.contains("`procs`"),
        "retained job discovery must name the deferred procs tool"
    );
    assert!(
        body.contains("procs(op = \"list\")"),
        "procs list form must be shown"
    );
    assert!(
        body.contains("procs(op = \"grep\""),
        "procs grep form must be shown"
    );
    assert!(
        body.contains("stays deferred until the first call names it"),
        "procs must be pinned deferred until first use"
    );
    for false_claim in [
        "activates on search",
        "search activates",
        "automatically activated",
        "always offered",
    ] {
        assert!(
            !body.contains(false_claim),
            "promotion prose must not claim {false_claim}"
        );
    }
    Ok(())
}

#[test]
fn skill_names_are_exact_and_fail_closed() -> Result<(), Box<dyn Error>> {
    let extension = skills()?;
    let records = extension.skills();
    let exact = |want: &str| records.iter().any(|skill| skill.name.as_str() == want);
    assert!(exact("find-anything"));
    assert!(exact("delegate-with-contracts"));
    assert!(exact("initializer-and-sprints"));
    for miss in [
        "find",
        "Find-Anything",
        "FIND-ANYTHING",
        "",
        "find-anything ",
    ] {
        assert!(
            !exact(miss),
            "lookup must be exact: {miss:?} resolves nothing"
        );
    }
    for bad in ["", "BadName", "-leading", "has space", "find-anything "] {
        assert!(
            Name::parse(bad).is_err(),
            "invalid skill name was accepted: {bad:?}"
        );
    }
    let too_long = "a".repeat(65);
    assert!(
        Name::parse(&too_long).is_err(),
        "overlong skill name was accepted"
    );
    Ok(())
}
