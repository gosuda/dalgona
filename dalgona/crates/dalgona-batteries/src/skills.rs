// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The bundled skills battery: three data-only skill packs as one extension.

use dal_core::ext::SkillRecord;
use dal_core::{Name, RegistrationError, ServiceSet};

/// The bundled `find-anything` skill body.
pub const FIND_ANYTHING_SKILL: &str = include_str!("skills/find-anything.md");

/// The bundled `delegate-with-contracts` skill body.
pub const DELEGATE_WITH_CONTRACTS_SKILL: &str = include_str!("skills/delegate-with-contracts.md");

/// The bundled `initializer-and-sprints` skill body.
pub const INITIALIZER_AND_SPRINTS_SKILL: &str = include_str!("skills/initializer-and-sprints.md");

const SKILLS: [(&str, &str, &str); 3] = [
    (
        "delegate-with-contracts",
        "Hand work to subagents with an objective, an output format, source guidance, and boundaries; collect files and references, not prose.",
        DELEGATE_WITH_CONTRACTS_SKILL,
    ),
    (
        "find-anything",
        "Pick the right search surface: find, grep, symbol, procs, web_search, or a deferred tool, and promote deferred tools correctly.",
        FIND_ANYTHING_SKILL,
    ),
    (
        "initializer-and-sprints",
        "Split a long build into one initializer pass and one feature per session, with a progress file each session reads before it plans.",
        INITIALIZER_AND_SPRINTS_SKILL,
    ),
];

/// The text of the `dalgona://skills` manual page.
pub const SKILLS_DOC: &str = concat!(
    "# skills\n\n",
    "The skills battery ships three bundled, data-only skills: find-anything,\n",
    "delegate-with-contracts, and initializer-and-sprints. A skill carries a\n",
    "description and a body; it declares no tools, services, or network access,\n",
    "and there is no second skill resolver. Name collisions with user plugins\n",
    "are errors, not guesses.\n",
);

fn skill(
    &(name, description, body): &(&str, &str, &str),
) -> Result<SkillRecord, RegistrationError> {
    Ok(SkillRecord {
        name: Name::parse(name)?,
        description: description.into(),
        body: body.into(),
        letter2image: false,
        mcp: None,
    })
}

/// Builds the bundled skills extension. Registration performs no I/O.
///
/// # Errors
/// Returns [`RegistrationError`] when the fixed identity or a skill record
/// is rejected.
pub fn skills() -> Result<dal_agent::ext::Extension, RegistrationError> {
    let mut builder = dal_agent::ext::ExtensionBuilder::new(
        "skills",
        env!("CARGO_PKG_VERSION"),
        ServiceSet::EMPTY,
    )?
    .with_origin(dal_core::Origin::Bundled, None);
    for entry in &SKILLS {
        builder = builder.skill(skill(entry)?);
    }
    builder.build()
}
