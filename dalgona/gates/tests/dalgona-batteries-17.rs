// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! This gate verifies bundled rule sets and their records.
#[path = "support/mod.rs"]
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{collections::BTreeSet, io};

use dal_core::Origin;
use dal_core::ext::{InterruptMode, RepeatMode, RuleRecord};

const RULES: [&str; 19] = [
    "atlas-v2-no-workaround",
    "atlas-v2-read-before-edit",
    "compact-cite-files",
    "compact-no-context-loss",
    "compact-state-on-disk",
    "docs-contract-markers",
    "docs-update-with-change",
    "git-commit-no-force-push",
    "git-commit-no-placeholder-message",
    "git-commit-no-secrets",
    "project-workflow-agents-md-binding",
    "project-workflow-slice-first",
    "project-workflow-write-it-down",
    "steer-no-apologies",
    "steer-no-meta",
    "steer-no-restate",
    "stop-act-dont-offer",
    "stop-evidence-before-done",
    "stop-finish-the-work",
];

const SCOPED: [(&str, &[&str], InterruptMode, u16); 15] = [
    (
        "atlas-v2-no-workaround",
        &["text"],
        InterruptMode::ProseOnly,
        10,
    ),
    (
        "compact-cite-files",
        &["text"],
        InterruptMode::ProseOnly,
        10,
    ),
    (
        "compact-no-context-loss",
        &["text"],
        InterruptMode::ProseOnly,
        5,
    ),
    (
        "docs-contract-markers",
        &["tool:patch"],
        InterruptMode::Never,
        10,
    ),
    (
        "git-commit-no-force-push",
        &["tool:exec"],
        InterruptMode::ToolOnly,
        1,
    ),
    (
        "git-commit-no-placeholder-message",
        &["tool:exec"],
        InterruptMode::ToolOnly,
        1,
    ),
    (
        "git-commit-no-secrets",
        &["tool:patch"],
        InterruptMode::ToolOnly,
        1,
    ),
    (
        "project-workflow-slice-first",
        &["text"],
        InterruptMode::ProseOnly,
        10,
    ),
    (
        "project-workflow-write-it-down",
        &["text"],
        InterruptMode::ProseOnly,
        5,
    ),
    (
        "steer-no-apologies",
        &["text"],
        InterruptMode::ProseOnly,
        10,
    ),
    ("steer-no-meta", &["text"], InterruptMode::ProseOnly, 10),
    ("steer-no-restate", &["text"], InterruptMode::ProseOnly, 10),
    (
        "stop-act-dont-offer",
        &["text"],
        InterruptMode::ProseOnly,
        5,
    ),
    (
        "stop-evidence-before-done",
        &["text"],
        InterruptMode::ProseOnly,
        5,
    ),
    (
        "stop-finish-the-work",
        &["text"],
        InterruptMode::ProseOnly,
        10,
    ),
];

const ALWAYS: [&str; 4] = [
    "atlas-v2-read-before-edit",
    "compact-state-on-disk",
    "docs-update-with-change",
    "project-workflow-agents-md-binding",
];

fn rules(product: &dal_agent::Product) -> support::TestResult<Vec<RuleRecord>> {
    let extension = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "ttsr-rules")
        .ok_or_else(|| io::Error::other("the product has no ttsr-rules extension"))?;
    assert_eq!(extension.origin(), Origin::Bundled);
    Ok(extension.rules().to_vec())
}

fn rule<'a>(rules: &'a [RuleRecord], name: &str) -> support::TestResult<&'a RuleRecord> {
    rules
        .iter()
        .find(|rule| rule.name.as_str() == name)
        .ok_or_else(|| io::Error::other(format!("{name} rule is missing")).into())
}

#[test]
fn bundled_rule_sets_gate() -> support::TestResult<()> {
    let scratch = support::Scratch::new("bundled-rule-sets")?;
    let data_root = scratch.path().to_path_buf();
    let product = support::build_product(data_root.clone(), None)?;
    assert!(product.bundled.is_empty());
    let all = rules(&product)?;
    let names: BTreeSet<&str> = all.iter().map(|rule| rule.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(RULES));
    for record in &all {
        assert!(!record.text.starts_with('\u{feff}'), "{}", record.name);
        assert!(!record.text.contains('\r'), "{}", record.name);
        assert!(record.text.ends_with('\n'), "{}", record.name);
        assert!(!record.text.starts_with("---\n"), "{}", record.name);
        assert!(record.enabled && !record.report, "{}", record.name);
    }
    for (name, tokens, mode, gap) in SCOPED {
        let record = rule(&all, name)?;
        let scope = record
            .scope
            .as_ref()
            .ok_or_else(|| io::Error::other(format!("{name} scope was not registered")))?;
        assert_eq!(scope.text, tokens.contains(&"text"), "{name}");
        assert!(!scope.thinking, "{name}");
        assert_eq!(
            scope.tool,
            tokens.iter().any(|token| token.starts_with("tool")),
            "{name}"
        );
        let named_tools: Vec<&str> = tokens
            .iter()
            .filter_map(|token| token.strip_prefix("tool:"))
            .collect();
        let actual: Vec<&str> = scope
            .named_tools
            .iter()
            .map(dal_core::Name::as_str)
            .collect();
        assert_eq!(actual, named_tools, "{name}");
        assert_eq!(record.mode, Some(mode), "{name}");
        assert_eq!(record.repeat_mode, Some(RepeatMode::AfterGap), "{name}");
        assert_eq!(record.repeat_gap, Some(gap), "{name}");
        assert!(!record.patterns.is_empty(), "{name}");
        assert!(!record.always_apply, "{name}");
    }
    for name in ALWAYS {
        let record = rule(&all, name)?;
        assert!(record.always_apply, "{name}");
        assert!(record.patterns.is_empty(), "{name}");
        assert_eq!(record.scope, None, "{name}");
        assert_eq!(record.mode, None, "{name}");
        assert_eq!(record.repeat_mode, None, "{name}");
        assert_eq!(record.repeat_gap, None, "{name}");
    }

    let selected = support::build_product(
        data_root.clone(),
        Some("[rule_sets]\nenabled = [\"steer\"]\n"),
    )?;
    let selected_names: BTreeSet<String> = rules(&selected)?
        .iter()
        .map(|rule| rule.name.to_string())
        .collect();
    assert_eq!(
        selected_names,
        BTreeSet::from([
            "steer-no-apologies".to_owned(),
            "steer-no-meta".to_owned(),
            "steer-no-restate".to_owned(),
        ])
    );

    let invalid = support::build_product(
        data_root,
        Some("[rule_sets]\nenabled = [\"steer\", \"nope\"]\n"),
    );
    let error = match invalid {
        Ok(_) => return Err(io::Error::other("an unknown rule set was accepted").into()),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("rule_sets.enabled"), "{error}");
    assert!(error.contains("nope"), "{error}");
    Ok(())
}
