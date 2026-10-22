// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Detector lanes fire on their fixtures through the live product.
use dalgona_gates::support;

use std::{fs, io};

use dalgona_batteries::quality::detectors::{
    collapse, control_leak, fabricated_call, repetitive_turns,
};

fn fixture(lane: &str, name: &str) -> support::TestResult<String> {
    let path = support::repo_root()
        .join("dalgona/gates/tests/fixtures/detectors")
        .join(lane)
        .join(name);
    Ok(fs::read_to_string(&path)?)
}

/// Proven-firing inputs taken from the detector unit tests: a dominant
/// punctuation run for collapse, a repeated control token for leaks. The
/// stored prose fixtures sit below the detector thresholds by design.
fn collapse_positive() -> String {
    "!".repeat(300)
}

fn control_positive() -> String {
    "<|endoftext|>".repeat(3)
}

/// Batteries other than quality stay off so the scripted steps below drive
/// exactly one provider response per model call.
const ONLY_QUALITY: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]";
/// The same isolation with the detectors rule set omitted.
const SILENCED: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"review\", \"skills\", \"ttsr-rules\", \"web\", \"work\"]\n[rule_sets]\nenabled = []";

/// Streams `text` as one model turn through the real binary and returns the
/// `rule_fired` records the turn left in its session journal.
fn fired_records(
    label: &str,
    text: &str,
    top_level_toml: &str,
) -> support::TestResult<Vec<String>> {
    let scratch = support::Scratch::new(label)?;
    let journal = support::run_scripted_print(
        &scratch,
        top_level_toml,
        &[support::text_step(text), support::text_step("done")],
        "speak",
    )?;
    // Detector fires journal as `ext` records whose kind names the event.
    Ok(support::journal_records(&journal, "ext")
        .into_iter()
        .filter(|record| record.contains(r#""kind":"rule_fired""#))
        .map(str::to_owned)
        .collect())
}

/// Lane units: each lane latches after one fire; clean input never fires.
fn lane_units_latch_and_stay_quiet(
    collapse_positive: &str,
    control_positive: &str,
) -> support::TestResult<()> {
    let mut collapse_state = collapse::State::new(collapse::Source::Prose);
    assert!(collapse_state.feed(collapse_positive).is_some());
    assert!(collapse_state.latched());
    assert!(collapse_state.feed(collapse_positive).is_none());
    let mut collapse_clean = collapse::State::new(collapse::Source::Prose);
    assert!(
        collapse_clean
            .feed(&fixture("collapse-repetition", "clean.txt")?)
            .is_none()
    );
    let mut control_state = control_leak::State::new();
    assert!(control_state.feed(control_positive).is_some());
    assert!(control_state.feed(control_positive).is_none());
    let mut control_clean = control_leak::State::new();
    assert!(
        control_clean
            .feed(&fixture("control-token-leak", "clean.txt")?)
            .is_none()
    );
    let repetitive_positive = fixture("repetitive-turns", "positive.txt")?;
    let mut turns = repetitive_turns::TurnHistory::restore(&[]);
    assert!(turns.commit(&repetitive_positive).is_none());
    assert!(turns.commit(&repetitive_positive).is_none());
    assert!(turns.commit(&repetitive_positive).is_some());
    let mut clean_turns = repetitive_turns::TurnHistory::restore(&[]);
    for turn in [
        "First inspect the contract. Then run the changed path and check the resulting state.",
        "Measure the render budget before drawing, and reuse stored letters when the grid matches.",
        "Summarize the diff for the reviewer, naming every finding with its file and severity.",
    ] {
        assert!(
            clean_turns.commit(turn).is_none(),
            "a dissimilar turn never reports: {turn}"
        );
    }
    Ok(())
}

/// The fabricated-call rule is registered on the live product with its
/// stable name, and withdrawn when the detectors set is omitted.
fn fabricated_call_rule_follows_the_detectors_set() -> support::TestResult<()> {
    let rule = fabricated_call::rule()?;
    assert_eq!(rule.name.as_str(), fabricated_call::RULE_NAME);
    assert_ne!(rule.patterns.len(), 0);
    let scratch = support::Scratch::new("detector-registration")?;
    let root = scratch.path().to_path_buf();
    let product = support::build_product(root.clone(), None)?;
    let quality = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "quality")
        .ok_or_else(|| io::Error::other("the product has no quality extension"))?;
    assert!(
        quality
            .rules()
            .iter()
            .any(|rule| rule.name.as_str() == fabricated_call::RULE_NAME)
    );
    drop(product);
    let without = support::build_product(root, Some("[rule_sets]\nenabled = []\n"))?;
    let quiet = without
        .extensions
        .iter()
        .find(|extension| extension.name() == "quality")
        .ok_or_else(|| io::Error::other("the product has no quality extension"))?;
    assert!(
        quiet
            .rules()
            .iter()
            .all(|rule| rule.name.as_str() != fabricated_call::RULE_NAME)
    );
    Ok(())
}

#[test]
fn detector_lanes_fire_once_on_fixtures_and_stay_quiet_on_clean_input() -> support::TestResult<()> {
    // A product that never calls its detectors leaves no record: the positive
    // inputs below must each produce exactly one firing for their lane.
    let collapse_positive = collapse_positive();
    let collapse_fires = fired_records("detector-collapse", &collapse_positive, ONLY_QUALITY)?;
    assert_eq!(
        collapse_fires.len(),
        1,
        "collapse prints exactly one firing: {collapse_fires:?}"
    );
    assert!(
        collapse_fires[0].contains("collapse-repetition"),
        "the firing names its lane: {}",
        collapse_fires[0]
    );
    let collapse_quiet = fired_records(
        "detector-collapse-clean",
        &fixture("collapse-repetition", "clean.txt")?,
        ONLY_QUALITY,
    )?;
    assert!(
        collapse_quiet.is_empty(),
        "clean input fires nothing: {collapse_quiet:?}"
    );

    let control_positive = control_positive();
    let control_fires = fired_records("detector-control", &control_positive, ONLY_QUALITY)?;
    assert_eq!(
        control_fires.len(),
        1,
        "control tokens print exactly one firing: {control_fires:?}"
    );
    assert!(
        control_fires[0].contains("control-token-leak"),
        "the firing names its lane: {}",
        control_fires[0]
    );

    // Omitting the detectors set quiets the same positive input end to end.
    let silenced = fired_records("detector-silenced", &collapse_positive, SILENCED)?;
    assert!(
        silenced.is_empty(),
        "without the detectors set nothing fires: {silenced:?}"
    );

    lane_units_latch_and_stay_quiet(&collapse_positive, &control_positive)?;
    fabricated_call_rule_follows_the_detectors_set()?;
    Ok(())
}
