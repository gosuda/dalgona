//! Semantic tests for the authored vendor seed rows in `catalog/builtin.rs`.
//!
//! These tests exercise catalog behavior only (resolution, override,
//! visibility, honest defaults). Wire binding and `UnboundModel` refusal
//! belong to the wire owner's slices and are not asserted here.

use super::*;
use crate::catalog::builtin::built_in_entries;
use crate::catalog::resolve::capability_row;
use crate::provider::{OPENAI, OPENAI_CODEX};

fn catalog_for(
    id: &str,
    family: Family,
    base_url: &str,
    entries: Vec<CatalogEntry>,
    source: CatalogSource,
) -> Catalog {
    Catalog::with_sources(vec![(provider(id, family, base_url), source)], entries)
}

fn seeded(catalog: &Catalog, entry_provider: &ProviderEntry, id: &str) -> Option<CatalogEntry> {
    capability_row(catalog.entries(), entry_provider, id).cloned()
}

/// Missing credential (`CatalogSource::Typed`, no live rows): the offline
/// catalog still serves the seeded Kimi ids, and exactly the vendor-published
/// numbers land on the ids the vendor documents.
#[test]
fn typed_kimi_catalog_serves_seeded_ids_with_documented_numbers() {
    let catalog = catalog_for(
        "kimi-coding",
        Family::Anthropic,
        "https://api.kimi.com/coding",
        Vec::new(),
        CatalogSource::Typed,
    );
    let entry_provider = provider(
        "kimi-coding",
        Family::Anthropic,
        "https://api.kimi.com/coding",
    );
    let row = seeded(&catalog, &entry_provider, "kimi-for-coding")
        .expect("vendor-documented id resolves offline");
    assert_eq!(row.context_window, Some(1_048_576));
    assert_eq!(row.max_output, None);
    assert!(row.image_input);
    let fork_fact = seeded(&catalog, &entry_provider, "kimi-k2-thinking")
        .expect("fork catalog fact stays seeded");
    assert_eq!(fork_fact.context_window, None);
    assert_eq!(fork_fact.max_output, None);
    assert!(!fork_fact.image_input);
}

/// A live row of the same `(provider, id)` replaces the seed exactly once;
/// every other seed of that provider survives.
#[test]
fn live_row_same_id_replaces_seed_and_other_seeds_survive() {
    let mut live = row("kimi-coding", "kimi-for-coding", Some(123));
    live.thinking = ThinkingSupport::Budget { can_disable: true };
    let catalog = Catalog::with_sources(
        vec![(
            provider(
                "kimi-coding",
                Family::Anthropic,
                "https://api.kimi.com/coding",
            ),
            CatalogSource::Live,
        )],
        vec![live],
    );
    let rows: Vec<_> = catalog
        .entries()
        .iter()
        .filter(|candidate| {
            candidate.provider.as_ref() == "kimi-coding"
                && candidate.id.as_ref() == "kimi-for-coding"
        })
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "live row replaces the seed without duplication"
    );
    assert_eq!(rows[0].context_window, Some(123));
    let entry_provider = provider(
        "kimi-coding",
        Family::Anthropic,
        "https://api.kimi.com/coding",
    );
    assert!(
        seeded(&catalog, &entry_provider, "k3").is_some(),
        "untouched seed survives the live override"
    );
}

/// `glm-5.3` served by `zai` never appears under `zhipu-coding-plan`, whose
/// own live row resolves separately; no cross-provider bleed in either
/// direction.
#[test]
fn seeded_ids_do_not_bleed_across_providers() {
    let live = row("zhipu-coding-plan", "glm-5.3", Some(555));
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("zai", Family::Chat, "https://api.z.ai/api/coding/paas/v4"),
                CatalogSource::Typed,
            ),
            (
                provider(
                    "zhipu-coding-plan",
                    Family::Chat,
                    "https://open.bigmodel.cn/api/coding/paas/v4",
                ),
                CatalogSource::Live,
            ),
        ],
        vec![live],
    );
    let zai = provider("zai", Family::Chat, "https://api.z.ai/api/coding/paas/v4");
    let zhipu = provider(
        "zhipu-coding-plan",
        Family::Chat,
        "https://open.bigmodel.cn/api/coding/paas/v4",
    );
    assert_eq!(
        seeded(&catalog, &zai, "glm-5.3")
            .expect("zai seed")
            .context_window,
        Some(1_048_576)
    );
    assert_eq!(
        seeded(&catalog, &zhipu, "glm-5.3")
            .expect("zhipu live row")
            .context_window,
        Some(555)
    );
}

/// Vendor-prefixed gateway ids stay on their provider: the prefixed id
/// resolves under the gateway only, other providers see nothing, and the
/// bare form does not match the prefixed seed.
#[test]
fn vendor_prefixed_gateway_ids_stay_on_their_provider() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider(
                    "vercel-ai-gateway",
                    Family::Anthropic,
                    "https://ai-gateway.vercel.sh",
                ),
                CatalogSource::Typed,
            ),
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let vercel = provider(
        "vercel-ai-gateway",
        Family::Anthropic,
        "https://ai-gateway.vercel.sh",
    );
    let anthropic = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    assert!(
        seeded(&catalog, &vercel, "anthropic/claude-opus-5").is_some(),
        "prefixed id resolves under the gateway"
    );
    assert!(
        seeded(&catalog, &anthropic, "anthropic/claude-opus-5").is_none(),
        "no bleed onto the vendor's own provider id"
    );
    assert!(
        seeded(&catalog, &vercel, "claude-opus-5").is_none(),
        "bare form does not match the prefixed seed"
    );
}

/// Vendor-published numbers land on exactly the ids the vendor documents;
/// undocumented siblings stay `None` instead of inheriting a sibling's
/// numbers.
#[test]
fn documented_numbers_land_on_documented_ids_only() {
    let zai = catalog_for(
        "zai",
        Family::Chat,
        "https://api.z.ai/api/coding/paas/v4",
        Vec::new(),
        CatalogSource::Typed,
    );
    let zai_entry = provider("zai", Family::Chat, "https://api.z.ai/api/coding/paas/v4");
    let glm = seeded(&zai, &zai_entry, "glm-5.3").expect("documented");
    assert_eq!(glm.context_window, Some(1_048_576));
    assert_eq!(glm.max_output, Some(131_072));
    assert!(!glm.image_input);
    let flash = seeded(&zai, &zai_entry, "glm-5.3-flash").expect("documented");
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output, Some(131_072));
    assert!(flash.image_input);
    let undocumented = seeded(&zai, &zai_entry, "glm-5.2-highspeed").expect("seeded");
    assert_eq!(undocumented.context_window, None);
    assert_eq!(undocumented.max_output, None);
    assert!(!undocumented.image_input);
    let cn = catalog_for(
        "zai-coding-cn",
        Family::Chat,
        "https://open.bigmodel.cn/api/coding/paas/v4",
        Vec::new(),
        CatalogSource::Typed,
    );
    let cn_entry = provider(
        "zai-coding-cn",
        Family::Chat,
        "https://open.bigmodel.cn/api/coding/paas/v4",
    );
    let vision = seeded(&cn, &cn_entry, "glm-4.6v").expect("documented");
    assert_eq!(vision.context_window, Some(131_072));
    assert!(vision.image_input);
}

/// The non-dispatchable Google id on `opencode-go` stays listed, not
/// hidden: it refuses at wire selection (`UnboundModel`, the wire owner's
/// slice), not at catalog visibility.
#[test]
fn catalog_only_google_id_stays_listed() {
    let catalog = catalog_for(
        "opencode-go",
        Family::Chat,
        "https://opencode.ai/zen/go/v1",
        Vec::new(),
        CatalogSource::Typed,
    );
    let go = provider("opencode-go", Family::Chat, "https://opencode.ai/zen/go/v1");
    let listed =
        seeded(&catalog, &go, "gemini-4-argon-contributor").expect("catalog-only id stays visible");
    assert_eq!(listed.listing, Listing::Listed);
}

/// Classifier ids stay census records: no chat seed row exists for them.
#[test]
fn classifier_ids_are_not_seeded() {
    let classifiers = built_in_entries()
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.id.as_ref(),
                "jev-1.13" | "jev-1.13-free" | "typesafe-ai/jev"
            )
        })
        .count();
    assert_eq!(classifiers, 0);
}

/// Seeds state no money and no unverifiable capability: price, premium, and
/// image billing stay unknown so budget estimation keeps its fail-closed
/// behavior, and every seed stays listed.
#[test]
fn seeds_carry_no_invented_numbers_or_prices() {
    for candidate in built_in_entries() {
        let seeded_row = candidate.provider.as_ref() != OPENAI.id
            && candidate.provider.as_ref() != OPENAI_CODEX.id;
        if seeded_row {
            assert_eq!(candidate.price, None, "{} must stay unpriced", candidate.id);
            assert_eq!(candidate.premium_multiplier, None, "{}", candidate.id);
            assert_eq!(candidate.image_profile, None, "{}", candidate.id);
            assert_eq!(candidate.listing, Listing::Listed, "{}", candidate.id);
            assert!(!candidate.temperature_allowed, "{}", candidate.id);
        }
    }
}

/// The authored set matches the approved census: exact counts per provider
/// with no duplicates.
#[test]
fn authored_seed_set_matches_the_approved_census() {
    let expected: &[(&str, usize)] = &[
        ("kimi-coding", 5),
        ("zai", 7),
        ("zai-coding-cn", 4),
        ("alibaba-token-plan", 21),
        ("opencode", 82),
        ("opencode-go", 31),
        ("vercel-ai-gateway", 297),
    ];
    let entries = built_in_entries();
    for (id, count) in expected {
        let ids: Vec<_> = entries
            .iter()
            .filter(|candidate| candidate.provider.as_ref() == *id)
            .map(|candidate| candidate.id.clone())
            .collect();
        assert_eq!(ids.len(), *count, "{id} row count");
        let unique: std::collections::BTreeSet<Box<str>> = ids.iter().cloned().collect();
        assert_eq!(unique.len(), *count, "{id} ids must be unique");
    }
    assert_eq!(
        entries
            .iter()
            .filter(|candidate| {
                let id = candidate.provider.as_ref();
                id != OPENAI.id && id != OPENAI_CODEX.id
            })
            .count(),
        447,
        "total authored seed rows"
    );
}
