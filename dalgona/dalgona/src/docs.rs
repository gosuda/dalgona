// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `dalgona://` manual, registered through the shared docs door.

use std::sync::Arc;

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Origin, RegistrationError, ServiceSet};
use dalgona_batteries::{
    ask, eval, history, judged, mcp, orchestration, procs, quality, review, skills, ttsr_rules,
    web, work,
};

const PAGES: &[(&str, &str)] = &[
    ("agents", orchestration::docs::AGENTS_DOC),
    ("ask", ask::ASK_DOC),
    ("batteries", include_str!("docs/batteries.md")),
    ("changelog", include_str!("docs/changelog.md")),
    ("config", include_str!("docs/config.md")),
    ("eval", eval::EVAL_DOC),
    ("goal", orchestration::docs::GOAL_DOC),
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
    ("workflows", orchestration::docs::WORKFLOWS_DOC),
    ("plan", work::PLAN_DOC),
    ("procs", procs::PROCS_DOC),
    ("todo", include_str!("docs/todo.md")),
    ("work", work::PLAN_DOC),
];

/// The orchestration page: the battery's static body plus its constants
/// table, generated from the same named constants the code enforces. Built
/// at registration, so no process-wide static holds it.
pub(crate) fn orchestration_page() -> String {
    let mut text = orchestration::ORCHESTRATION_DOC.to_owned();
    text.push_str(&orchestration::constants_table());
    text
}

fn title(text: &str) -> &str {
    text.lines()
        .next()
        .map_or("", |line| line.trim_start_matches('#').trim())
}

/// The `dalgona://symbols` page: a pointer to the shared reference, not a copy.
///
/// Symbol search is documented once in the dal manual. This path stays so
/// `dalgona://symbols` keeps resolving, but its body points at
/// `dal://symbols` instead of duplicating it.
const SYMBOLS_POINTER: &str = "# Symbols\n\nThe symbol-search reference lives in the dal manual: read `dal://symbols` (`dalgon docs dal://symbols`).\n";

fn pages() -> Vec<(String, String)> {
    let mut pages: Vec<(String, String)> = PAGES
        .iter()
        .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
        .collect();
    if let Some(page) = pages.iter_mut().find(|(path, _)| path == "orchestration") {
        page.1 = orchestration_page();
    }
    pages.push(("symbols".to_owned(), SYMBOLS_POINTER.to_owned()));
    pages
}

/// Builds the extension that serves every `dalgona://<page>` document.
///
/// # Errors
/// Returns a registration error when a page path or the identity is invalid.
pub(crate) fn extension() -> Result<Extension, RegistrationError> {
    let mut builder =
        ExtensionBuilder::new(crate::NAME, env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
            .with_origin(Origin::Builtin, None);
    for (path, text) in &pages() {
        builder = builder.doc(path, title(text), text);
    }
    builder
        .scheme(
            crate::NAME,
            Arc::new(dalgon::ManualScheme::new(crate::NAME)),
        )
        .build()
}
pub(crate) fn manuals() -> Vec<dalgon::ProductManual> {
    let mut manuals = dalgon::product::builtin_manuals();
    let mut pages: Vec<_> = pages();
    pages.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    manuals.push(dalgon::ProductManual {
        scheme: String::from(crate::NAME),
        plugin: String::from(crate::NAME),
        pages,
    });
    manuals.sort_by(|left, right| left.scheme.as_bytes().cmp(right.scheme.as_bytes()));
    manuals
}
