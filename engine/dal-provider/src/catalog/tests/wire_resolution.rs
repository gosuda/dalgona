//! Wire-aware resolution: the static wire table decides binding, family,
//! and refusal. Unbound ids stay listed but refuse typed, an explicit `api`
//! narrows the row, an explicit `base_url` refuses a truly fixed-endpoint
//! wire while the BAI Anthropic wire derives from that base, the bare search
//! follows wire families, and the dot/hyphen alias never leaves its own row.

use super::*;
use crate::provider::wire::WireSelect;

/// The built-in row `id`, failing the test when the row is absent so a
/// renamed row reads as a fixture error, never as a resolution one.
fn row_def(id: &str) -> &'static crate::provider::ProviderDef {
    crate::find(id).unwrap_or_else(|| panic!("built-in row {id} missing from the table"))
}

/// An entry for one built-in row, keeping the row's own family and base.
fn row_entry(id: &str) -> ProviderEntry {
    let def = row_def(id);
    provider(def.id, def.family, def.base_url)
}

/// One catalog holding `entry` as its only provider, with the checked-in
/// seed rows plus `extra` rows at `source`.
fn catalog_with(entry: ProviderEntry, source: CatalogSource, extra: Vec<CatalogEntry>) -> Catalog {
    Catalog::with_sources(vec![(entry, source)], extra)
}

/// The dot/hyphen twin of `id`, swapping every `.` with `-` and vice versa.
fn twin(id: &str) -> String {
    id.bytes()
        .map(|byte| match byte {
            b'.' => b'-',
            b'-' => b'.',
            other => other,
        })
        .map(|byte| byte as char)
        .collect()
}

#[test]
fn a_wired_row_refuses_an_unbound_id_typed_while_it_stays_listed() {
    let entry = row_entry("opencode");
    let catalog = catalog_with(
        entry,
        CatalogSource::Live,
        vec![row("opencode", "unbound-id", None)],
    );
    let listed = catalog
        .entries()
        .iter()
        .any(|row| row.provider.as_ref() == "opencode" && row.id.as_ref() == "unbound-id");
    assert!(listed, "a live unbound id stays visible in the catalog");
    let error = resolve(&catalog, &[], "opencode/unbound-id").unwrap_err();
    assert_eq!(
        error.to_string(),
        "provider opencode serves no wire for model unbound-id"
    );
    // The Typed fallback is disabled for wired rows: no live row, same
    // refusal instead of a family-default row.
    let typed = catalog_with(row_entry("opencode"), CatalogSource::Typed, Vec::new());
    assert_eq!(
        resolve(&typed, &[], "opencode/unbound-id")
            .unwrap_err()
            .to_string(),
        "provider opencode serves no wire for model unbound-id"
    );
}

#[test]
fn a_wired_row_binds_the_selected_wire_family() {
    let catalog = catalog_with(row_entry("opencode"), CatalogSource::Typed, Vec::new());
    let claude = resolve(&catalog, &[], "opencode/claude-opus-5").expect("claude binds");
    match &claude.route {
        ModelRoute::Api { family, .. } => {
            assert_eq!(*family, Family::Anthropic, "the Anthropic wire binds");
        }
        other => panic!("opencode/claude-opus-5 resolved to {other:?}"),
    }
    assert!(
        matches!(claude.entry.thinking, ThinkingSupport::UnknownAdaptive),
        "an Anthropic-wire id carries the Anthropic thinking contract"
    );
    let glm = resolve(&catalog, &[], "opencode/glm-5.3").expect("glm binds");
    match &glm.route {
        ModelRoute::Api { family, .. } => assert_eq!(*family, Family::Chat),
        other => panic!("opencode/glm-5.3 resolved to {other:?}"),
    }
    assert!(matches!(glm.entry.thinking, ThinkingSupport::OpenAi { .. }));
}

#[test]
fn an_explicit_api_narrows_a_wired_row_to_its_configured_family() {
    let mut entry = row_entry("opencode");
    entry.origins.api = Origin::Explicit;
    let catalog = catalog_with(entry, CatalogSource::Typed, Vec::new());
    let error = resolve(&catalog, &[], "opencode/claude-opus-5").unwrap_err();
    assert_eq!(
        error.to_string(),
        "provider opencode does not serve the API family of model claude-opus-5"
    );
    let same_family = resolve(&catalog, &[], "opencode/glm-5.3");
    assert!(
        same_family.is_ok(),
        "the configured family still binds: {same_family:?}"
    );
}

#[test]
fn an_explicit_bai_base_serves_all_three_wire_families() {
    let mut entry = row_entry("bai");
    entry.base_url = "https://staging.b.ai/v1".into();
    entry.origins.base = Origin::Explicit;
    let catalog = catalog_with(entry, CatalogSource::Typed, Vec::new());
    // Pinned `bai.ts` lines 101 to 104: the custom gateway base serves Chat
    // and Responses verbatim and Anthropic at the same base with one
    // trailing `/v1` removed; the request wire re-adds `/v1/messages`, so
    // the same operator host keeps every wire's `/v1/...` path.
    let anthropic =
        resolve(&catalog, &[], "bai/claude-sonnet-5").expect("the BAI Anthropic wire derives");
    match &anthropic.route {
        ModelRoute::Api { family, .. } => {
            assert_eq!(*family, Family::Anthropic, "the Anthropic wire binds");
        }
        other => panic!("bai/claude-sonnet-5 resolved to {other:?}"),
    }
    let chat = resolve(&catalog, &[], "bai/gemini-3.8-flash").expect("the BAI Chat wire binds");
    match &chat.route {
        ModelRoute::Api { family, .. } => assert_eq!(*family, Family::Chat),
        other => panic!("bai/gemini-3.8-flash resolved to {other:?}"),
    }
    let responses =
        resolve(&catalog, &[], "bai/gpt-5.6-sol").expect("the BAI Responses wire binds");
    match &responses.route {
        ModelRoute::Api { family, .. } => assert_eq!(*family, Family::Responses),
        other => panic!("bai/gpt-5.6-sol resolved to {other:?}"),
    }
    // No remaining built-in Override wire is truly fixed: `bai` is the only
    // row carrying `WireBase::Override`, and its Anthropic wire derives as
    // above. `FixedWireBase` stays for future non-derivable Overrides.
}

#[test]
fn the_bare_search_follows_wire_families_and_the_narrowing() {
    let mut narrowed = row_entry("opencode");
    narrowed.origins.api = Origin::Explicit;
    let route = ModelRoute::Api {
        family: Family::Anthropic,
        model: "claude-opus-5".into(),
        provider: None,
    };
    let open = catalog_with(row_entry("opencode"), CatalogSource::Typed, Vec::new());
    let resolved = resolve_route(&open, &[], &route).expect("the Anthropic wire serves bare");
    match &resolved.route {
        ModelRoute::Api { family, .. } => assert_eq!(*family, Family::Anthropic),
        other => panic!("bare claude-opus-5 resolved to {other:?}"),
    }
    // With the row narrowed by an explicit `api`, the wire family no longer
    // serves the bare search: the id stays unknown to the family.
    let closed = catalog_with(narrowed, CatalogSource::Typed, Vec::new());
    assert_eq!(
        resolve_route(&closed, &[], &route).unwrap_err().to_string(),
        "unknown model claude-opus-5"
    );
}

#[test]
fn a_provider_route_refuses_an_unbound_model_even_in_the_row_family() {
    let catalog = catalog_with(row_entry("opencode"), CatalogSource::Live, Vec::new());
    let route = ModelRoute::Api {
        family: row_def("opencode").family,
        model: "unbound-id".into(),
        provider: Some("opencode".into()),
    };
    assert_eq!(
        resolve_route(&catalog, &[], &route)
            .unwrap_err()
            .to_string(),
        "provider opencode serves no wire for model unbound-id"
    );
}

#[test]
fn dot_hyphen_twins_reach_rows_on_an_aliasing_row_only() {
    let def = row_def("bai");
    let entry = row_entry("bai");
    let Some(aliased) = def.wires.iter().find_map(|wire| match wire.select {
        WireSelect::Exact(id) if id.bytes().any(|byte| byte == b'-' || byte == b'.') => {
            Some(id.to_owned())
        }
        _ => None,
    }) else {
        panic!("the bai row pins an Exact selector with a dot or hyphen");
    };
    let flipped = twin(&aliased);
    assert_ne!(flipped, aliased);
    // The alias finds the catalog row across the spelling: capability
    // lookup on the queried twin returns the row spelled the other way.
    let rows = vec![{
        let mut row = row("bai", &aliased, None);
        row.context_window = Some(555);
        row
    }];
    let found = capability_row(&rows, &entry, &flipped).expect("the twin reaches the row");
    assert_eq!(found.id.as_ref(), aliased, "the row keeps its own spelling");
    // Exact beats twin when the row lists both spellings.
    let both = vec![rows[0].clone(), {
        let mut row = row("bai", &flipped, None);
        row.context_window = Some(777);
        row
    }];
    let found = capability_row(&both, &entry, &flipped).expect("the exact row binds");
    assert_eq!(found.context_window, Some(777), "the exact spelling wins");
    // No provider bleed: the twin never reaches another row's rows.
    let other_entry = row_entry("opencode");
    assert!(capability_row(&rows, &other_entry, &flipped).is_none());
    // And the alias never creates a wire: a twin that selects no wire is
    // refused typed exactly like any other unbound id.
    let catalog = catalog_with(entry, CatalogSource::Typed, Vec::new());
    let bound = crate::provider::wire::wire_for(
        catalog
            .providers
            .first()
            .map(|(entry, _)| entry)
            .expect("one entry"),
        &flipped,
    );
    let resolved = resolve(&catalog, &[], &format!("bai/{flipped}"));
    if bound.is_some() {
        let resolved = resolved.expect("a wire-selected twin binds");
        assert_eq!(
            resolved.entry.id.as_ref(),
            flipped,
            "the queried spelling is kept"
        );
    } else {
        assert_eq!(
            resolved.unwrap_err().to_string(),
            format!("provider bai serves no wire for model {flipped}")
        );
    }
}

#[test]
fn rows_without_wires_and_named_entries_keep_today_resolution() {
    // A single-wire row (empty wire table): the Typed fallback survives.
    let openai_def = row_def("openai");
    assert!(
        openai_def.wires.is_empty(),
        "the openai row keeps the single-wire contract"
    );
    let typed = catalog_with(
        provider("openai", Family::Responses, "https://api.openai.com/v1"),
        CatalogSource::Typed,
        Vec::new(),
    );
    assert!(resolve(&typed, &[], "openai/brand-new-id").is_ok());
    let cached = catalog_with(
        provider("openai", Family::Responses, "https://api.openai.com/v1"),
        CatalogSource::Cache,
        Vec::new(),
    );
    assert_eq!(
        resolve(&cached, &[], "openai/brand-new-id")
            .unwrap_err()
            .to_string(),
        "unknown model openai/brand-new-id"
    );
    // A named entry carries no row and binds its family plainly.
    let mut named = provider("acme", Family::Chat, "https://acme.test/v1");
    named.def = None;
    let catalog = catalog_with(named, CatalogSource::Typed, Vec::new());
    assert!(resolve(&catalog, &[], "acme/anything").is_ok());
}
