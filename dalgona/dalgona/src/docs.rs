// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The `dalgona://` manual, registered through the shared docs door.

use std::sync::Arc;

use dal_agent::ext::{Extension, ExtensionBuilder};
use dal_core::{Origin, RegistrationError, ServiceSet};
use dalgona_batteries::{
    ask, eval, history, judged, mcp, orchestration, quality, review, skills, ttsr_rules, web, work,
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
    ("plan", include_str!("docs/plan.md")),
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

#[expect(
    clippy::expect_used,
    reason = "the generated dal manual must contain the shared symbols page"
)]
static DAL_SYMBOLS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    dalgon::product::builtin_manuals()
        .into_iter()
        .find(|manual| manual.scheme == "dal")
        .and_then(|manual| {
            manual
                .pages
                .into_iter()
                .find(|(page, _)| page == "symbols")
                .map(|(_, text)| text)
        })
        .expect("generated dal manual must contain the symbols page")
});

fn pages() -> Vec<(String, String)> {
    let mut pages: Vec<(String, String)> = PAGES
        .iter()
        .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
        .collect();
    if let Some(page) = pages.iter_mut().find(|(path, _)| path == "orchestration") {
        page.1 = orchestration_page();
    }
    pages.push(("symbols".to_owned(), DAL_SYMBOLS.as_str().to_owned()));
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
