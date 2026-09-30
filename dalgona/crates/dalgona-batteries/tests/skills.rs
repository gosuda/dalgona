// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Behavior test for the bundled skills extension.

use std::error::Error;

use dal_core::Origin;
use dalgona_batteries::skills::skills;

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
