// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `dalgona://` manual, registered through the shared docs door.

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Origin, RegistrationError, ServiceSet};
use dalgona_batteries::{ask, history, judged, mcp, orchestration, quality, review, skills, ttsr_rules, web, work};

const PAGES: &[(&str, &str)] = &[
    ("ask", ask::ASK_DOC),
    ("batteries", include_str!("docs/batteries.md")),
    ("changelog", include_str!("docs/changelog.md")),
    ("config", include_str!("docs/config.md")),
    ("history", history::HISTORY_DOC),
    ("judge", judged::JUDGED_DOC),
    ("judged", judged::JUDGED_DOC),
    ("mcp", mcp::MCP_DOC),
    ("orchestration", orchestration::ORCHESTRATION_DOC),
    ("anti-slop", quality::QUALITY_DOC),
    ("philosophy", include_str!("docs/philosophy.md")),
    ("quality", quality::QUALITY_DOC),
    ("review", review::REVIEW_DOC),
    ("rules", ttsr_rules::RULES_DOC),
    ("ttsr/rules", ttsr_rules::RULES_DOC),
    ("skills", skills::SKILLS_DOC),
    ("search", include_str!("docs/search.md")),
    ("web", web::WEB_DOC),
    ("plan", include_str!("docs/plan.md")),
    ("todo", include_str!("docs/todo.md")),
    ("work", work::PLAN_DOC),
];

fn title(text: &str) -> &str {
    text.lines()
        .next()
        .map_or("", |line| line.trim_start_matches('#').trim())
}

/// Builds the extension that serves every `dalgona://<page>` document.
///
/// # Errors
/// Returns a registration error when a page path or the identity is invalid.
pub(crate) fn extension() -> Result<Extension, RegistrationError> {
    let mut builder = ExtensionBuilder::new(crate::NAME, env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
        .with_origin(Origin::Builtin, None);
    for (path, text) in PAGES {
        builder = builder.doc(path, title(text), text);
    }
    builder.build()
}
pub(crate) fn manuals() -> Vec<dalgon::ProductManual> {
    let mut manuals = dalgon::product::builtin_manuals();
    let mut pages: Vec<_> = PAGES
        .iter()
        .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
        .collect();
    pages.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    manuals.push(dalgon::ProductManual {
        scheme: String::from("dalgona"),
        plugin: String::from("dalgona"),
        pages,
    });
    manuals.sort_by(|left, right| left.scheme.as_bytes().cmp(right.scheme.as_bytes()));
    manuals
}
