//! TTSR rule-pack gates.
// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
use std::collections::BTreeSet;
use std::error::Error;
use std::io;

use dal_core::Name;
use dal_core::ext::RuleRecord;
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

#[test]
fn each_pack_registers_its_rules() -> Result<(), Box<dyn Error>> {
    let expected = [
        ("steer", 3),
        ("compact", 3),
        ("stop", 3),
        ("docs", 2),
        ("atlas-v2", 2),
        ("project-workflow", 3),
        ("git-commit", 3),
    ];
    assert_eq!(expected.map(|(set, _)| set).as_slice(), PACKS);
    for (set, count) in expected {
        assert_eq!(rules_of(&[set])?.len(), count, "{set}");
    }
    assert_eq!(rules_of(&[])?.len(), 0);
    assert_eq!(rules_of(&["detectors"])?.len(), 0);
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
    assert_eq!(conditions, 15);
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
    assert!(matches!(
        error,
        ttsr_rules::RuleSetsError::UnknownSet(name) if name == "nope"
    ));
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
