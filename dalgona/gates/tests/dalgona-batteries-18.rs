// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
use dalgona_gates::support;

use std::{collections::BTreeSet, io, time::Duration};

use dal_core::Origin;

const SKILLS: [&str; 3] = [
    "delegate-with-contracts",
    "find-anything",
    "initializer-and-sprints",
];

fn skills(product: &dal_agent::Product) -> support::TestResult<Vec<dal_core::ext::SkillRecord>> {
    let extension = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "skills")
        .ok_or_else(|| io::Error::other("the product has no skills extension"))?;
    assert_eq!(extension.origin(), Origin::Bundled);
    assert!(
        extension.inject().is_empty(),
        "a skill pack declares no service"
    );
    assert!(extension.tools().is_empty() && extension.commands().is_empty());
    Ok(extension.skills().to_vec())
}

/// Checks that an unknown-skill failure names the missing skill and every
/// loaded alternative in byte order, without leaking any body. The live
/// names are byte-sorted here so the check asserts resolver order, not the
/// extension's record declaration order.
fn assert_missing_skill_names_alternatives<'a, I>(text: &str, skills: I) -> Result<(), io::Error>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    if !text.contains("nope") {
        return Err(io::Error::other(format!(
            "failure text omits the missing name: {text}"
        )));
    }
    let mut live: Vec<(&str, &str)> = skills.into_iter().collect();
    live.sort_unstable();
    let mut offset = 0;
    for (name, body) in live {
        let Some(at) = text[offset..].find(name) else {
            return Err(io::Error::other(format!("failure text omits {name}")));
        };
        offset += at + name.len();
        if text.contains(body) {
            return Err(io::Error::other("failure text must not leak a skill body"));
        }
    }
    Ok(())
}

#[test]
fn skills_and_search_defaults_gate() -> support::TestResult<()> {
    let scratch = support::Scratch::new("skills-search-defaults")?;
    let data_root = scratch.path().to_path_buf();
    let default_product = support::build_product(data_root.clone(), None)?;
    let records = skills(&default_product)?;
    let names: BTreeSet<&str> = records.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(SKILLS));
    for skill in &records {
        assert!(!skill.description.is_empty(), "{}", skill.name);
        assert!(skill.description.chars().count() <= 300, "{}", skill.name);
        assert!(!skill.letter2image, "{}", skill.name);
        assert!(!skill.body.starts_with('\u{feff}'), "{}", skill.name);
        assert!(!skill.body.contains('\r'), "{}", skill.name);
        assert!(
            skill.body.ends_with('\n') && !skill.body.ends_with("\n\n"),
            "{}",
            skill.name
        );
        assert!(skill.body.lines().count() <= 200, "{}", skill.name);
        for forbidden in ["dal.tool", "dal.on", "ctx.", "credential"] {
            assert!(
                !skill.body.contains(forbidden),
                "{} contains {forbidden}",
                skill.name
            );
        }
    }

    let overridden = support::build_product(
        data_root.clone(),
        Some("search_symbols = false\nedit_style = \"anchor\"\n"),
    )?;
    assert_eq!(skills(&overridden)?, records);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(data_root).await?;
        let body = records
            .iter()
            .find(|skill| skill.name.as_str() == "initializer-and-sprints")
            .ok_or_else(|| io::Error::other("initializer skill is missing"))?
            .body
            .clone();
        let doc = host.doc("skill://initializer-and-sprints")?;
        assert!(format!("{doc:?}").contains(&format!("{:?}", &*body)));
        // An unknown skill URI names the missing skill and every loaded
        // alternative in byte order, leaking no body.
        let text = host
            .doc("skill://nope")
            .err()
            .ok_or_else(|| io::Error::other("an unknown skill URI must fail"))?
            .to_string();
        let alternatives = records
            .iter()
            .map(|skill| (skill.name.as_str(), &*skill.body));
        assert_missing_skill_names_alternatives(&text, alternatives)?;
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
