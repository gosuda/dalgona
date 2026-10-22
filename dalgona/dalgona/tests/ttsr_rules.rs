// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! TTSR rule-pack gates.
use std::collections::BTreeSet;
use std::error::Error;
use std::io;
use std::sync::Arc;

use dal_core::Name;
use dal_core::RulesConfig;
use dal_core::ext::RuleRecord;
use dal_ext::ttsr::Severity;
use dal_ext::ttsr::build::{self, RuleBuildInput};
use dal_ext::ttsr::matcher::{self, Compiled, StreamState};
use dal_ext::ttsr::record::RecordSource;
use dalgona_batteries::ttsr_rules::{self, DEFAULT_ENABLED, DETECTOR_RULES, PACKS, SETS};
use regex::Regex;

fn rules_of(enabled: &[&str]) -> Result<Vec<RuleRecord>, Box<dyn Error>> {
    let enabled: Vec<String> = enabled.iter().map(|name| (*name).to_owned()).collect();
    let extension = ttsr_rules::ttsr_rules(&enabled)?;
    Ok(extension.rules().to_vec())
}

fn rule(rules: &[RuleRecord], name: &str) -> Result<Regex, Box<dyn Error>> {
    let record = rules
        .iter()
        .find(|rule| rule.name.as_str() == name)
        .ok_or_else(|| io::Error::other(format!("missing rule {name}")))?;
    let pattern = record
        .patterns
        .first()
        .ok_or_else(|| io::Error::other(format!("rule {name} has no pattern")))?;
    Ok(Regex::new(pattern)?)
}

/// One discriminant sample per pattern rule, keyed only by its pack.
/// The pack must match its own samples and no other pack may match them.
/// Moving a pattern rule across packs, even with a compensating dummy that
/// preserves every count, breaks the origin or the destination, so this
/// catches misplacements through firing behavior with no rule-name roster.
const PACK_BEHAVIOR: &[(&str, &str)] = &[
    ("steer", "I apologize"),
    ("steer", "as an AI language model"),
    ("steer", "let me restate the task"),
    ("compact", "as mentioned earlier"),
    ("compact", "I cannot recall"),
    ("stop", "shall I continue"),
    ("stop", "all tests pass"),
    ("stop", "I'll leave the rest"),
    ("docs", "+ version 2.0.0 released"),
    ("docs", "+ deprecated shim"),
    ("atlas-v2", "this is a quick fix"),
    ("project-workflow", "rewrite the whole module"),
    ("project-workflow", "I'll keep that in mind"),
    ("git-commit", "git push --force origin main"),
    ("git-commit", "git commit -m \"wip\""),
    ("git-commit", "api_key = \"abcdefgh12345678\""),
];

/// Texts no pack may match: near-misses and ordinary prose.
const PACK_MISSES: &[&str] = &[
    "turn right",
    "as mentioned",
    "I'll finish the rest myself",
    "fix: real message",
    "this is a proper fix",
    "rewrite the function",
    "git commit -m \"fix: real message\"",
    "plain prose with ordinary words",
    "they are absolutely right",
    "the implementation is correct",
];

fn record_matches(record: &RuleRecord, text: &str) -> Result<bool, Box<dyn Error>> {
    for pattern in &record.patterns {
        if Regex::new(pattern)?.is_match(text) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rules_match(rules: &[RuleRecord], text: &str) -> Result<bool, Box<dyn Error>> {
    for record in rules {
        if record_matches(record, text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn pack_matches(pack: &str, text: &str) -> Result<bool, Box<dyn Error>> {
    rules_match(&rules_of(&[pack])?, text)
}

fn pack_matches_with(pack: &str, extra: &str, text: &str) -> Result<bool, Box<dyn Error>> {
    rules_match(&rules_of(&[pack, extra])?, text)
}

fn any_other_pack_matches(own: &str, text: &str) -> Result<bool, Box<dyn Error>> {
    for &other in PACKS {
        if other == own {
            continue;
        }
        if pack_matches(other, text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn any_pack_matches(text: &str) -> Result<bool, Box<dyn Error>> {
    for &pack in PACKS {
        if pack_matches(pack, text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[test]
fn each_pack_registers_its_rules() -> Result<(), Box<dyn Error>> {
    let mut covered = BTreeSet::new();
    for &(pack, _) in PACK_BEHAVIOR {
        covered.insert(pack);
    }
    let packs: BTreeSet<&str> = PACKS.iter().copied().collect();
    assert_eq!(covered, packs, "behavior table must cover every pack");
    for &(pack, sample) in PACK_BEHAVIOR {
        assert!(
            pack_matches(pack, sample)?,
            "{pack} did not match {sample:?}"
        );
        assert!(
            !any_other_pack_matches(pack, sample)?,
            "another pack matched {pack}'s {sample:?}"
        );
    }
    for &miss in PACK_MISSES {
        assert!(!any_pack_matches(miss)?, "a pack matched miss {miss:?}");
    }
    assert!(
        rules_of(&[])?.is_empty(),
        "selecting no sets must register no rules"
    );
    assert!(
        rules_of(&["detectors"])?.is_empty(),
        "the detectors set registers no pack rules here"
    );
    for &(pack, sample) in PACK_BEHAVIOR {
        assert!(
            pack_matches_with(pack, "detectors", sample)?,
            "{pack} + detectors lost {sample:?}"
        );
    }
    Ok(())
}

#[test]
fn rule_names_and_conditions_are_valid_and_unique() -> Result<(), Box<dyn Error>> {
    let all = rules_of(SETS)?;
    assert_eq!(all.len(), 19);
    let mut names = BTreeSet::new();
    let mut conditions = 0;
    for record in &all {
        assert!(
            names.insert(record.name.to_string()),
            "duplicate {}",
            record.name
        );
        assert!(record.patterns.len() <= 16, "{}", record.name);
        assert!(record.text.len() <= 12_288, "{}", record.name);
        for pattern in &record.patterns {
            Regex::new(pattern)?;
            conditions += 1;
        }
    }
    assert_eq!(conditions, 16);
    for name in DETECTOR_RULES {
        assert!(
            Name::parse(name).is_ok(),
            "invalid detector rule name {name}"
        );
        assert!(
            names.insert((*name).to_owned()),
            "detector rule {name} collides with a pack rule"
        );
    }
    assert_eq!(names.len(), 23);
    Ok(())
}

#[test]
fn unknown_set_is_rejected_with_the_set_list() -> Result<(), Box<dyn Error>> {
    let enabled = ["steer".to_owned(), "nope".to_owned()];
    let error = ttsr_rules::validate_enabled(&enabled)
        .err()
        .ok_or_else(|| io::Error::other("an unknown rule set was accepted"))?;
    let message = error.to_string();
    assert!(matches!(
        error,
        ttsr_rules::RuleSetsError::UnknownSet(name) if name == "nope"
    ));
    let (first, hint) = message
        .split_once('\n')
        .ok_or_else(|| io::Error::other("the unknown-set error is not two lines"))?;
    assert_eq!(
        first,
        "dalgona: dal.toml: rule_sets.enabled [\"nope\"] is invalid"
    );
    let listed: Vec<&str> = hint
        .strip_prefix("Use a set name from ")
        .and_then(|rest| rest.strip_suffix('.'))
        .ok_or_else(|| io::Error::other(format!("unexpected hint: {hint:?}")))?
        .split(", ")
        .collect();
    assert_eq!(listed, SETS, "the hint names every shipped set, in order");
    Ok(())
}

#[test]
fn default_empty_and_duplicate_lists_validate() -> Result<(), Box<dyn Error>> {
    let mut defaults: Vec<String> = DEFAULT_ENABLED
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    ttsr_rules::validate_enabled(&defaults)?;
    ttsr_rules::validate_enabled(&[])?;
    defaults.push("steer".to_owned());
    ttsr_rules::validate_enabled(&defaults)?;
    assert_eq!(ttsr_rules::ttsr_rules(&defaults)?.rules().len(), 19);
    Ok(())
}

#[test]
fn docs_contract_markers_attack_their_distance_and_version_bounds() -> Result<(), Box<dyn Error>> {
    let rules = rules_of(&["docs"])?;
    let record = rules
        .iter()
        .find(|record| record.name.as_str() == "docs-contract-markers")
        .ok_or_else(|| io::Error::other("the docs pack has no docs-contract-markers rule"))?;
    let fires = |text: &str| record_matches(record, text);
    let filler = |count: usize| "x".repeat(count);

    for keyword in ["version 1.2.3", "deprecated", "breaking change"] {
        assert!(
            fires(&format!("+{} {keyword}", filler(199)))?,
            "199 + blank"
        );
        assert!(
            !fires(&format!("+{} {keyword}", filler(200)))?,
            "{keyword}: 200 + blank is one past the 200-character window"
        );
        assert!(
            fires(&format!("+{} {keyword}", filler(198)))?,
            "{keyword}: 198 + blank"
        );
        assert!(
            !fires(&format!("+{}\n{keyword}", filler(10)))?,
            "{keyword}: a newline ends the window"
        );
    }

    for (text, expected) in [
        ("+ version 1.2.3", true),
        ("+ VERSION 1.2.3", true),
        ("+ version 1234.123.123", true),
        ("+ version 12345.1.1", false),
        ("+ version 1.1234.1", false),
        ("+ version 1.1.1234", false),
        ("+ version 1.2", false),
        ("+ version 1.2.", false),
        ("+ version v1.2.3", false),
        ("version 1.2.3", false),
        ("- version 1.2.3", false),
        ("+deprecated", true),
        ("+ undeprecated", false),
        ("+ deprecatedly", false),
        ("+ a + deprecated", true),
        ("+ Breaking Change", true),
        ("+ breaking-change", false),
    ] {
        assert_eq!(fires(text)?, expected, "{text:?}");
    }
    Ok(())
}

#[test]
fn quote_and_escape_conditions_match_their_samples() -> Result<(), Box<dyn Error>> {
    let all = rules_of(SETS)?;
    let cases = [
        ("git-commit-no-placeholder-message", "git commit -m \"wip\""),
        ("git-commit-no-secrets", "api_key = \"abcdefgh12345678\""),
        ("stop-finish-the-work", "I'll leave the rest"),
        ("project-workflow-write-it-down", "I'll keep that in mind"),
    ];
    let negative = "git commit -m \"fix: real message\"";
    for (name, sample) in cases {
        let regex = rule(&all, name)?;
        assert!(regex.is_match(sample), "{name} did not match its sample");
        assert!(
            !regex.is_match(negative),
            "{name} matched the negative sample"
        );
    }
    let apology = rule(&all, "steer-no-apologies")?;
    for (text, expected) in [
        ("I apologize", true),
        ("My apologies", true),
        ("Sorry for the confusion", true),
        ("Sorry for the mistake", true),
        ("You're right", true),
        ("You are absolutely right", true),
        ("turn right", false),
        ("they are absolutely right", false),
        ("the implementation is correct", false),
    ] {
        assert_eq!(apology.is_match(text), expected, "{text:?}");
    }
    Ok(())
}

/// A deterministic xorshift64* generator: reproducible property randomness
/// with no new dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        let remainder = self.next_u64() % bound as u64;
        usize::try_from(remainder).unwrap_or_default()
    }
}

const TEXT_PIECES: &[&str] = &[
    "I apologize",
    "My apologies",
    "sorry for the confusion",
    "You're absolutely right",
    "turn right",
    "as an AI language model",
    "I do not have access to",
    "let me restate the task",
    "as mentioned earlier",
    "I cannot recall",
    "I lost the earlier context",
    "shall I continue",
    "all tests pass",
    "everything works",
    "I'll leave the rest",
    "the rest is straightforward",
    "workaround",
    "quick fix",
    "git push --force origin main",
    "git push -f origin main",
    "git commit -m \"wip\"",
    "git commit -m \"fix: real message\"",
    "api_key = \"abcdefgh12345678\"",
    "password: \"ZZZZZZZZZZZZZZZZZ\"",
    "<unavailable-tool-call>",
    "[called tool 'patch' (no longer available in this session)]",
    "+ version 1.2.3 in the changelog",
    "+ deprecated shim",
    "plain prose with ordinary words",
    "counts 12345 and deadbeefcafe0123",
    "punctuation !!! ??? ...",
    "\n",
    " ",
    "\t",
];

fn random_text(rng: &mut Rng) -> String {
    let mut text = String::new();
    let pieces = 1 + rng.below(8);
    for _ in 0..pieces {
        text.push_str(TEXT_PIECES[rng.below(TEXT_PIECES.len())]);
        text.push(' ');
    }
    text
}

/// Byte offsets the test feeds at, in order: random boundaries in 1 to 5
/// character steps, always ending at the text end.
fn random_splits(rng: &mut Rng, text: &str) -> Vec<usize> {
    let boundaries: Vec<usize> = text
        .char_indices()
        .skip(1)
        .map(|(index, _)| index)
        .collect();
    let mut splits = Vec::new();
    let mut at = 0usize;
    while at < boundaries.len() {
        at = (at + 1 + rng.below(5)).min(boundaries.len());
        splits.push(boundaries[at - 1]);
    }
    splits.push(text.len());
    splits
}

fn oracle_for(pattern: &str) -> Result<(Regex, Arc<Compiled>), Box<dyn Error>> {
    let oracle = Regex::new(pattern)?;
    let compiled =
        Arc::new(matcher::compile(pattern, 0).map_err(|skip| io::Error::other(skip.to_string()))?);
    Ok((oracle, compiled))
}

/// Byte offset of the first prefix that the whole-buffer oracle matches.
fn first_matching_end(oracle: &Regex, text: &str) -> Option<usize> {
    (1..=text.len()).find(|end| text.is_char_boundary(*end) && oracle.is_match(&text[..*end]))
}

/// Feeds one text in random deltas; returns where the first fire landed.
///
/// The fire's feed must be the one that contains the first matching prefix
/// of the whole-buffer oracle.
fn feed_splits(
    state: &mut StreamState,
    text: &str,
    splits: &[usize],
    first_match_end: Option<usize>,
) -> Option<usize> {
    let mut fires = Vec::new();
    let mut start = 0usize;
    for stop in splits.iter().copied() {
        state.feed(&text.as_bytes()[start..stop], &mut fires);
        if !fires.is_empty() {
            if let Some(first) = first_match_end {
                assert!(
                    start < first && first <= stop,
                    "fired at {stop}, first match at {first} on {text:?}"
                );
            }
            return Some(stop);
        }
        start = stop;
    }
    None
}

#[test]
fn pack_conditions_fire_exactly_when_the_whole_text_matches() -> Result<(), Box<dyn Error>> {
    let all = rules_of(SETS)?;
    let mut rng = Rng::new(0x0D41_600A);
    let mut checked = 0usize;
    for record in &all {
        for pattern in &record.patterns {
            let (oracle, compiled) = oracle_for(pattern)?;
            for _ in 0..48 {
                let text = random_text(&mut rng);
                let expected = oracle.is_match(&text);
                let first_match_end = if expected {
                    Some(first_matching_end(&oracle, &text).ok_or_else(|| {
                        io::Error::other("the whole text matches but no prefix does")
                    })?)
                } else {
                    None
                };
                let mut state = StreamState::new();
                state.add_rule(0, std::slice::from_ref(&compiled));
                let fired_at = feed_splits(
                    &mut state,
                    &text,
                    &random_splits(&mut rng, &text),
                    first_match_end,
                );
                assert_eq!(fired_at.is_some(), expected, "{pattern} on {text:?}");
                checked += 1;
            }
        }
    }
    assert!(checked >= 500, "only {checked} property cases ran");
    Ok(())
}

/// One mebibyte of degenerate stream text per shape: a single repeated
/// byte, a repeated near-match fragment, a long unbroken run after a
/// bounded-gap anchor, and NUL bytes.
fn degenerate_streams() -> Vec<Vec<u8>> {
    const MIB: usize = 1 << 20;
    let repeat = |unit: &str| {
        let mut bytes = unit.as_bytes().repeat(MIB / unit.len() + 1);
        bytes.truncate(MIB);
        bytes
    };
    vec![
        repeat("a"),
        repeat(" "),
        repeat("\n"),
        repeat("git push "),
        repeat("I apologize "),
        repeat("+deprecated"),
        repeat("api_key=\"A"),
        repeat("<unavailable-tool-"),
        vec![0; MIB],
        {
            let mut bytes = b"+ ".to_vec();
            bytes.extend(std::iter::repeat_n(b'x', MIB));
            bytes
        },
    ]
}

/// Feeds one stream in 4 KiB deltas; asserts at most one fire and bounded excerpts.
fn assert_stream_survives(compiled: &Arc<Compiled>, pattern: &str, stream: &[u8]) {
    let mut state = StreamState::new();
    state.add_rule(0, std::slice::from_ref(compiled));
    let mut fires = Vec::new();
    for delta in stream.chunks(4096) {
        state.feed(delta, &mut fires);
    }
    assert!(
        fires.len() <= 1,
        "{pattern} fired {} times on one stream",
        fires.len()
    );
    for fire in &fires {
        assert!(
            fire.excerpt.len() <= matcher::EXCERPT_BYTES,
            "{pattern} excerpt of {} bytes",
            fire.excerpt.len()
        );
    }
    assert!(
        state.excerpt().len() <= matcher::EXCERPT_BYTES,
        "{pattern} ring grew past its bound"
    );
}

/// Runs every degenerate stream against one pattern; returns the run count.
fn assert_pattern_survives(pattern: &str, streams: &[Vec<u8>]) -> Result<usize, Box<dyn Error>> {
    let (_, compiled) = oracle_for(pattern)?;
    for stream in streams {
        assert_stream_survives(&compiled, pattern, stream);
    }
    Ok(streams.len())
}

#[test]
fn pack_conditions_survive_megabyte_degenerate_streams() -> Result<(), Box<dyn Error>> {
    let all = rules_of(SETS)?;
    let streams = degenerate_streams();
    let mut runs = 0usize;
    for record in &all {
        for pattern in &record.patterns {
            runs += assert_pattern_survives(pattern, &streams)?;
        }
    }
    assert!(runs >= 100, "only {runs} degenerate runs ran");
    Ok(())
}

/// Builds one rule snapshot through the session load path with every
/// bundled pack and the detector text rule selected, and asserts that no
/// condition was skipped.
#[test]
fn every_bundled_rule_loads_with_zero_skipped_conditions() -> Result<(), Box<dyn Error>> {
    let enabled: Vec<String> = SETS.iter().map(|name| (*name).to_owned()).collect();
    let extension = ttsr_rules::ttsr_rules(&enabled)?;
    let plugin = Name::parse("ttsr-rules")?;
    let quality = Name::parse("quality")?;
    let mut records: Vec<RecordSource> = extension
        .rules()
        .iter()
        .map(|record| RecordSource {
            plugin: plugin.clone(),
            site: None,
            record: record.clone(),
        })
        .collect();
    records.push(RecordSource {
        plugin: quality,
        site: None,
        record: dalgona_batteries::quality::detectors::fabricated_call::rule()?,
    });
    let known_tools = ["exec", "patch"];
    let input = RuleBuildInput {
        records: &records,
        plugin_rules: &[],
        known_tools: &known_tools,
        agent: "main",
    };
    let unique = format!("dalgona-ttsr-load-{}", std::process::id());
    let data_root = std::env::temp_dir().join(format!("{unique}-data"));
    let workspace = std::env::temp_dir().join(format!("{unique}-ws"));
    let set = build::set_for(&input, &data_root, &workspace, &RulesConfig::default());
    let skipped: Vec<&str> = set
        .problems
        .iter()
        .filter(|problem| problem.severity == Severity::Skipped)
        .map(|problem| problem.reason.as_str())
        .collect();
    assert!(
        skipped.is_empty(),
        "the bundled packs skipped conditions at load: {skipped:?}"
    );
    Ok(())
}

/// Cross-checks the shipped `docs-contract-markers` rewrite against the
/// retired combined pattern: the split two-condition form must accept
/// exactly the texts the retired single condition accepted.
#[test]
fn docs_condition_rewrite_is_language_equivalent() {
    let retired = r"(?i)(?:\+.{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b)|(?:\+.{0,200}\b(?:deprecated|breaking change)\b)";
    let rewritten: &[&str] = &[
        r"(?i)\+[^\n+]{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b",
        r"(?i)\+[^\n+]{0,200}\b(?:deprecated|breaking change)\b",
    ];
    let retired = Regex::new(retired).expect("the retired condition compiles");
    let rewritten: Vec<Regex> = rewritten
        .iter()
        .map(|pattern| Regex::new(pattern).expect("the rewritten condition compiles"))
        .collect();
    let alphabet: &[&str] = &[
        "+",
        "a",
        "b",
        " ",
        "\n",
        "version",
        " ",
        "1.2.3",
        "deprecated",
        "breaking change",
        "deprecated\n",
        "+version 1.2.3",
        "x",
        "xversion 2.0.0",
        " version 2.0.0",
        "Version",
        "VERSION 1.0.0",
    ];
    let mut rng = Rng::new(0x0D41_600B);
    let mut matched = 0usize;
    for _ in 0..4000 {
        let pieces = 1 + rng.below(6);
        let mut text = String::new();
        for _ in 0..pieces {
            text.push_str(alphabet[rng.below(alphabet.len())]);
            text.push([' ', '\n', '+'][rng.below(3)]);
        }
        let expected = retired.is_match(&text);
        let actual = rewritten.iter().any(|regex| regex.is_match(&text));
        assert_eq!(
            expected, actual,
            "the rewritten conditions disagree on {text:?}"
        );
        matched += usize::from(expected);
    }
    assert!(matched >= 50, "only {matched} equivalence cases matched");

    // The random corpus never reaches the 200-byte bound, so pin it: the
    // gap boundary and a newline inside the gap must keep old and new in
    // exact agreement. Each gap ends in a space so `\bversion` sees a word
    // boundary and the gap length is exact.
    let gap_text = |gap: &str| format!("+{gap}version 1.2.3");
    let gap_of = |len: usize| "x".repeat(len - 1) + " ";
    for (gap, matches) in [
        (gap_of(199), true),
        (gap_of(200), true),
        (gap_of(201), false),
        ("x".repeat(100) + "\n" + &gap_of(99), false),
    ] {
        let text = gap_text(&gap);
        let old = retired.is_match(&text);
        let new = rewritten.iter().any(|regex| regex.is_match(&text));
        assert_eq!(old, matches, "the retired condition misjudges {text:?}");
        assert_eq!(new, matches, "the rewritten conditions misjudge {text:?}");
    }
    for text in [
        "+xversion 1.2.3",
        "+Version 1.2.3",
        "+deprecated",
        "+breaking change",
        "+ x deprecated shim",
        "+breaking\nchange",
    ] {
        let old = retired.is_match(text);
        let new = rewritten.iter().any(|regex| regex.is_match(text));
        assert_eq!(old, new, "the rewritten conditions disagree on {text:?}");
    }
}
