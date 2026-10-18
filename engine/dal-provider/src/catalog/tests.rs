use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use super::*;
use super::{
    cache::{CacheError, read_cache_async, write_cache_async},
    decode::{
        decode_anthropic_page, decode_codex_models, decode_copilot_models, decode_devin_models,
        decode_mistral_models, decode_ollama_models, decode_openai_models, decode_radius_models,
    },
    prices::{PRICE_ROWS, PRICE_TIER_ROWS, TierRow, tier_rows_are_sorted_unique},
    resolve::capability_row,
};
use crate::{
    auth::credential::{OAuthCredential, SecretString},
    error::ResolveError,
    family::devin::proto::{
        ClientModelConfig, GetCliModelConfigsResponse, ModelFeatures, ModelInfo,
    },
    provider::{AuthStyle, ProviderSettings, Transport},
    thinking::{BeforeRequestPatch, Effort},
};
use prost::Message as _;

mod mistral_discovery;
mod ollama_discovery;

fn provider(id: &str, family: Family, base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        def: crate::find(id),
        family,
        base_url: base_url.into(),
        transport: Transport::Https,
        key_env: None,
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 1,
        settings: ProviderSettings::default(),
    }
}

fn row(provider: &str, id: &str, context_window: Option<u32>) -> CatalogEntry {
    CatalogEntry {
        provider: provider.into(),
        id: id.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window,
        max_output: None,
        thinking: ThinkingSupport::UnknownAdaptive,
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
        premium_multiplier: None,
        price: None,
    }
}

#[test]
fn aliases_expand_once_without_alias_chains() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://example.test/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "model", Some(42))],
    );
    let aliases = vec![
        (Box::<str>::from("first"), Box::<str>::from("second")),
        (Box::<str>::from("second"), Box::<str>::from("openai/model")),
    ];
    assert_eq!(
        resolve(&catalog, &aliases, "first")
            .unwrap_err()
            .to_string(),
        "unknown model second"
    );
}

#[test]
fn qualified_cache_miss_is_unknown_but_typed_source_accepts_new_ids() {
    let cached = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "known-model", Some(10_000))],
    );
    assert_eq!(
        resolve(&cached, &[], "openai/some-new-id")
            .unwrap_err()
            .to_string(),
        "unknown model openai/some-new-id"
    );
    assert_eq!(
        resolve(&cached, &[], "opneai/gpt-6-luna")
            .unwrap_err()
            .to_string(),
        "unknown model opneai/gpt-6-luna"
    );

    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let resolved = resolve(&typed, &[], "openai/some-new-id")
        .expect("provider without a list accepts typed ids");
    assert_eq!(resolved.entry.context_window, None);
}

#[test]
fn route_resolution_retries_an_unlisted_id_under_its_family_provider() {
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6".into(),
    };
    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    assert!(resolve(&typed, &[], "gpt-6").is_err(), "bare id is unknown");
    let resolved = resolve_route(&typed, &[], &route).expect("family provider serves the id");
    assert_eq!(resolved.provider.as_ref(), "openai");
    assert_eq!(resolved.route, route);

    let cached = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "known-model", Some(10_000))],
    );
    assert_eq!(
        resolve_route(&cached, &[], &route).unwrap_err().to_string(),
        "unknown model gpt-6",
        "a listed catalog still rejects an id it does not list",
    );
}

#[test]
fn route_retry_serves_every_family_through_its_single_owner() {
    let cases = [
        (
            Family::Bedrock,
            "my-bedrock",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        (Family::Cursor, "my-cursor", "https://api.cursor.com"),
        (Family::Devin, "my-devin", "https://server.codeium.com"),
        (
            Family::Droid,
            "factory-droid",
            "https://api.factory.ai",
        ),
        (Family::GitLab, "gitlab-duo", "https://gitlab.com"),
        (
            Family::GoogleGenerativeAi,
            "google",
            "https://generativelanguage.googleapis.com/v1beta",
        ),
        (
            Family::GoogleVertex,
            "google-vertex",
            "https://aiplatform.googleapis.com",
        ),
        (Family::GoogleGeminiCli, "my-gemini", "http://127.0.0.1:1"),
        (
            Family::GoogleAntigravity,
            "my-antigravity",
            "http://127.0.0.1:1",
        ),
    ];
    for (family, id, base) in cases {
        let typed = Catalog::with_sources(vec![(provider(id, family, base), CatalogSource::Typed)], Vec::new());
        let route = ModelRoute::Api {
            family,
            model: "some-new-id".into(),
        };
        assert!(
            resolve(&typed, &[], "some-new-id").is_err(),
            "{id}: bare id stays unknown on a typed catalog",
        );
        let resolved = resolve_route(&typed, &[], &route)
            .unwrap_or_else(|error| panic!("{id}: single owner serves a typed id: {error:?}"));
        assert_eq!(resolved.provider.as_ref(), id);
        assert_eq!(resolved.route, route);
        let cached = Catalog::with_sources(
            vec![(provider(id, family, base), CatalogSource::Cache)],
            vec![row(id, "known-model", Some(10_000))],
        );
        assert_eq!(
            resolve_route(&cached, &[], &route).unwrap_err().to_string(),
            "unknown model some-new-id",
            "{id}: a listed catalog still rejects an id it does not list",
        );
    }
}

#[test]
fn route_retry_never_guesses_among_same_family_providers() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("ollama", Family::Ollama, "http://127.0.0.1:11434"),
                CatalogSource::Typed,
            ),
            (
                provider("ollama-cloud", Family::Ollama, "https://ollama.com"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let route = ModelRoute::Api {
        family: Family::Ollama,
        model: "gpt-oss:120b".into(),
    };
    assert!(
        resolve(&catalog, &[], "ollama-cloud/gpt-oss:120b").is_ok(),
        "qualified cloud id resolves",
    );
    assert_eq!(
        resolve_route(&catalog, &[], &route).unwrap_err().to_string(),
        "unknown model gpt-oss:120b",
        "two same-family owners mean no family-prefix guess",
    );
}

#[test]
fn route_retry_uses_the_catalogs_only_owner_including_custom_ids() {
    for (id, base) in [
        ("ollama", "http://127.0.0.1:11434"),
        ("my-ollama", "http://10.0.0.9:11434"),
    ] {
        let typed =
            Catalog::with_sources(vec![(provider(id, Family::Ollama, base), CatalogSource::Typed)], Vec::new());
        let route = ModelRoute::Api {
            family: Family::Ollama,
            model: "some-new-id".into(),
        };
        assert!(
            resolve(&typed, &[], "some-new-id").is_err(),
            "{id}: bare id stays unknown on a typed catalog",
        );
        let resolved = resolve_route(&typed, &[], &route)
            .unwrap_or_else(|error| panic!("{id}: single owner serves a typed id: {error:?}"));
        assert_eq!(resolved.provider.as_ref(), id);
        assert_eq!(resolved.route, route);
    }
}

#[test]
fn route_resolution_prefers_the_listed_bare_id() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Typed,
            ),
            (
                provider("acme", Family::Chat, "https://acme.test/v1"),
                CatalogSource::Live,
            ),
        ],
        vec![row("acme", "gpt-6", Some(42))],
    );
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "gpt-6".into(),
    };
    let resolved = resolve_route(&catalog, &[], &route).expect("listed id resolves");
    assert_eq!(resolved.provider.as_ref(), "acme");
}

#[test]
fn route_resolution_disambiguates_a_shared_id_by_family() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("acme", Family::Chat, "https://acme.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("acme", "shared", Some(1)),
            row("anthropic", "shared", Some(2)),
        ],
    );
    assert!(matches!(
        resolve(&catalog, &[], "shared"),
        Err(ResolveError::AmbiguousModel { .. })
    ));
    for (family, owner) in [(Family::Chat, "acme"), (Family::Anthropic, "anthropic")] {
        let route = ModelRoute::Api {
            family,
            model: "shared".into(),
        };
        let resolved = resolve_route(&catalog, &[], &route).expect("family picks the owner");
        assert_eq!(resolved.provider.as_ref(), owner);
        assert_eq!(resolved.route, route);
    }
}

#[test]
fn route_resolution_never_returns_another_familys_provider() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Cache,
            ),
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Typed,
            ),
        ],
        vec![row("anthropic", "only-claude", Some(2))],
    );
    let route = ModelRoute::Api {
        family: Family::Chat,
        model: "only-claude".into(),
    };
    assert_eq!(
        resolve_route(&catalog, &[], &route)
            .unwrap_err()
            .to_string(),
        "unknown model only-claude",
    );
    let alias = [("pick".into(), "anthropic/only-claude".into())];
    let aliased = ModelRoute::Api {
        family: Family::Chat,
        model: "pick".into(),
    };
    assert!(resolve_route(&catalog, &alias, &aliased).is_err());
}

#[test]
fn bare_id_collision_names_candidates_in_catalog_order() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("one", Family::Responses, "https://one.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("two", Family::Anthropic, "https://two.test"),
                CatalogSource::Cache,
            ),
        ],
        vec![row("one", "shared", None), row("two", "shared", None)],
    );
    assert_eq!(
        resolve(&catalog, &[], "shared").unwrap_err().to_string(),
        "model id shared matches several providers: one/shared, two/shared"
    );
}

#[test]
fn first_slash_splits_provider_and_preserves_vendor_path() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
            CatalogSource::Cache,
        )],
        vec![row("zenmux", "openai/gpt-5.6-luna", None)],
    );
    let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
        .expect("qualified z-mux model resolves");
    assert_eq!(resolved.route.id(), "openai/gpt-5.6-luna");
    assert_eq!(resolved.entry.context_window, None);
    let aliases = [(
        Box::<str>::from("fast"),
        Box::<str>::from("zenmux/openai/gpt-5.6-luna"),
    )];
    assert_eq!(
        resolve(&catalog, &aliases, "fast")
            .expect("configured alias resolves once")
            .route
            .id(),
        "openai/gpt-5.6-luna"
    );
}

#[test]
fn typed_vendor_segment_uses_last_capability_candidate() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
        .expect("typed vendor-prefixed id uses its built-in capability row");
    assert_eq!(resolved.entry.context_window, Some(922_000));
}

#[test]
fn date_suffix_uses_the_base_capability_row() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        vec![row("anthropic", "claude-sonnet-5", Some(200_000))],
    );
    let resolved = resolve(&catalog, &[], "claude-sonnet-5-20260101")
        .expect("dated bare id matches its base row");
    assert_eq!(resolved.route.id(), "claude-sonnet-5-20260101");
    assert_eq!(resolved.entry.id.as_ref(), "claude-sonnet-5");
    assert_eq!(resolved.entry.context_window, Some(200_000));
}

#[test]
fn capability_lookup_prefers_exact_row_without_context_window() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let rows = [
        row("anthropic", "claude-sonnet-5", Some(200_000)),
        row("anthropic", "claude-sonnet-5-20260101", None),
    ];
    let matched = capability_row(&rows, &provider, "claude-sonnet-5-20260101")
        .expect("exact row with unknown context wins over the base row");
    assert_eq!(matched.id.as_ref(), "claude-sonnet-5-20260101");
    assert_eq!(matched.context_window, None);
}

#[test]
fn typed_resolution_uses_exact_compiled_temperature_pair() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Typed,
            ),
            (
                provider("zenmux", Family::Anthropic, "https://zenmux.test"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let supported = resolve(&catalog, &[], "anthropic/claude-haiku-4-5")
        .expect("known compiled model resolves as typed");
    let wrong_provider = resolve(&catalog, &[], "zenmux/claude-haiku-4-5")
        .expect("known provider accepts typed ids");
    let unknown = resolve(&catalog, &[], "anthropic/claude-haiku-5")
        .expect("unknown model ids remain routable");
    assert!(supported.entry.temperature_allowed);
    assert!(!wrong_provider.entry.temperature_allowed);
    assert!(!unknown.entry.temperature_allowed);
}

#[test]
fn reserve_is_a_hidden_codex_row_with_luna_limits() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider(
                    "openai-codex",
                    Family::Codex,
                    "https://chatgpt.com/backend-api/codex",
                ),
                CatalogSource::Typed,
            ),
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Typed,
            ),
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let reserve = resolve(&catalog, &[], "gpt-reserve").expect("hidden reserve row resolves");
    assert_eq!(reserve.provider.as_ref(), "openai-codex");
    assert_eq!(reserve.entry.listing, Listing::Hidden);
    assert_eq!(reserve.entry.display.as_ref(), "Luna Reserve");
    assert_eq!(reserve.entry.context_window, Some(258_400));
    assert!(reserve.entry.supports_reasoning_summaries);
    assert_eq!(
        resolve(&catalog, &[], "openai/gpt-reserve")
            .unwrap_err()
            .to_string(),
        "unknown model openai/gpt-reserve"
    );
    assert_eq!(
        resolve(&catalog, &[], "anthropic/gpt-reserve")
            .unwrap_err()
            .to_string(),
        "unknown model anthropic/gpt-reserve"
    );
}

#[test]
fn built_in_tool_support_is_api_and_effort_specific() {
    let entries = built_in_entries();
    let find = |id: &str| {
        entries
            .iter()
            .find(|entry| entry.id.as_ref() == id)
            .expect("built-in row exists")
    };
    let astra = find("gpt-6-astra").tool_support;
    assert_eq!(astra, ToolSupport::ResponsesOnly);
    assert!(!astra.allows(Family::Chat, ThinkingLevel::Off));
    assert!(astra.allows(Family::Responses, ThinkingLevel::High));

    let sol = find("gpt-6-sol").tool_support;
    assert_eq!(sol, ToolSupport::ChatWhenNoReasoning);
    assert!(sol.allows(Family::Chat, ThinkingLevel::Off));
    assert!(!sol.allows(Family::Chat, ThinkingLevel::High));
    assert!(sol.allows(Family::Responses, ThinkingLevel::High));

    let gpt_56 = find("gpt-5.6-luna").tool_support;
    assert_eq!(gpt_56, ToolSupport::Any);
    assert!(gpt_56.allows(Family::Chat, ThinkingLevel::High));
}

#[test]
fn openai_decoder_uses_exact_compiled_temperature_capability() {
    let provider = provider("openai", Family::Chat, "https://api.openai.com/v1");
    let rows = decode_openai_models(
        &provider,
        br#"{"data":[{"id":"gpt-4o"},{"id":"gpt-6-sol"}]}"#,
    )
    .expect("OpenAI model rows decode");
    assert!(rows[0].temperature_allowed);
    assert!(!rows[1].temperature_allowed);
}

#[test]
fn openai_catalog_row_exposes_its_complete_image_profile() {
    let provider = provider("openai", Family::Chat, "https://api.openai.com/v1");
    let rows = decode_openai_models(&provider, br#"{"data":[{"id":"gpt-6-sol"}]}"#)
        .expect("OpenAI model row decodes");
    assert_eq!(rows[0].image_profile, Some(ImageProfile::openai()));
}

#[test]
fn codex_decoder_hides_non_listed_rows_and_applies_window_percent() {
    let provider = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    let bytes = br#"{"models":[{"slug":"gpt-6-luna","display_name":"GPT-6 Luna","visibility":"list","context_window":1000,"effective_context_window_percent":95,"supports_reasoning_summaries":true,"supported_reasoning_levels":["low",{"effort":"high"}],"input_modalities":["text","image"]},{"slug":"gpt-reserve","display_name":"Luna Reserve","visibility":"hide","context_window":1000}]}"#;
    let rows = decode_codex_models(&provider, bytes).expect("Codex rows decode");
    assert_eq!(rows[0].context_window, Some(950));
    assert_eq!(rows[0].listing, Listing::Listed);
    assert!(rows[0].image_input);
    assert!(rows[0].supports_reasoning_summaries);
    assert_eq!(rows[1].listing, Listing::Hidden);
    assert_eq!(rows[1].context_window, Some(950));
    assert!(!rows[1].supports_reasoning_summaries);
}

#[test]
fn anthropic_decoder_reads_limits_and_capabilities() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"claude-sonnet-5","display_name":"Claude Sonnet 5","max_input_tokens":200000,"max_tokens":8192,"capabilities":{"image_input":{"supported":true},"thinking":{"types":{"adaptive":{"supported":true},"disabled":{"supported":true}}},"effort":{"supported":true,"high":{"supported":true},"low":{"supported":true},"max":{"supported":true},"medium":{"supported":true},"xhigh":{"supported":true}},"context_management":{"compact_20260112":{"supported":true}}}}],"has_more":true,"last_id":"claude-sonnet-5"}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert!(page.has_more);
    assert_eq!(page.last_id.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(page.rows[0].context_window, Some(200_000));
    assert_eq!(page.rows[0].max_output, Some(8192));
    assert!(page.rows[0].image_input);
    assert!(page.rows[0].remote_compact);
    if let ThinkingSupport::Adaptive {
        can_disable,
        accepted,
    } = &page.rows[0].thinking
    {
        assert!(*can_disable);
        assert_eq!(
            accepted,
            &[
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max,
            ]
        );
    } else {
        panic!("page did not report adaptive thinking");
    }
}

#[test]
fn anthropic_catalog_row_exposes_a_complete_profile_only_with_a_known_window() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"vision","max_input_tokens":200000,"capabilities":{"image_input":{"supported":true}}}]}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert_eq!(
        page.rows[0].image_profile,
        Some(ImageProfile::anthropic_standard(200_000))
    );
}

#[test]
fn image_capability_without_known_billing_window_has_no_profile() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"vision","capabilities":{"image_input":{"supported":true}}}]}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert!(page.rows[0].image_input);
    assert_eq!(page.rows[0].image_profile, None);
}

#[test]
fn live_decoders_replace_control_characters_in_ids_and_display_names() {
    let chat = provider("openai", Family::Chat, "https://api.openai.com/v1");
    let rows = decode_openai_models(
        &chat,
        r#"{"data":[{"id":"ok\u000eevil"}, {"id":"csi\u001b[2Jmodel\u0007tail"}, {"id":"osc\u001b]52;;c2VjcmV0\u0007model"}]}"#.as_bytes(),
    )
    .expect("OpenAI model rows decode");
    assert_eq!(rows[0].id.as_ref(), "ok�evil");
    assert_eq!(rows[1].id.as_ref(), "csi�[2Jmodel�tail");
    assert_eq!(rows[2].id.as_ref(), "osc�]52;;c2VjcmV0�model");

    let codex = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    let rows = decode_codex_models(
        &codex,
        r#"{"models":[{"slug":"plain","display_name":"N\u001b]52;;c2VjcmV0\u0007ame","visibility":"list"}]}"#
            .as_bytes(),
    )
    .expect("Codex rows decode");
    assert_eq!(rows[0].id.as_ref(), "plain");
    assert_eq!(rows[0].display.as_ref(), "N�]52;;c2VjcmV0�ame");

    let anthropic = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let page = decode_anthropic_page(
        &anthropic,
        r#"{"data":[{"id":"cl\naude","display_name":"Cl\u007faude"}],"has_more":false,"last_id":"x"}"#.as_bytes(),
    )
    .expect("Anthropic page decodes");
    assert_eq!(page.rows[0].id.as_ref(), "cl�aude");
    assert_eq!(page.rows[0].display.as_ref(), "Cl�aude");

    for row in rows.iter().chain(&page.rows) {
        for value in [&row.id, &row.display] {
            assert!(
                value.chars().all(|character| !character.is_control()),
                "{value}"
            );
        }
    }
}

#[tokio::test]
async fn poisoned_cache_rows_are_sanitized_on_read() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut poisoned = row("openai", "clean", Some(1000));
    poisoned.id = "evil\nid\u{1b}]0;spoof\u{7}tail".into();
    poisoned.display = "Nice\u{80}Name".into();
    write_cache_async(cache_path.clone(), "openai".into(), vec![poisoned])
        .await
        .expect("write poisoned cache row");
    let entries = read_cache_async(cache_path)
        .await
        .expect("poisoned cache still reads");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id.as_ref(), "evil�id�]0;spoof�tail");
    assert_eq!(entries[0].display.as_ref(), "Nice�Name");
}

#[test]
fn anthropic_decoder_uses_exact_compiled_temperature_capability() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"claude-haiku-4-5"},{"id":"claude-sonnet-5"}],"has_more":false,"last_id":"claude-sonnet-5"}"#;
    let page = decode_anthropic_page(&provider, bytes)
        .expect("Anthropic model rows decode without extra capability fields");
    assert!(page.rows[0].temperature_allowed);
    assert!(!page.rows[1].temperature_allowed);
}

#[test]
fn anthropic_effort_rows_keep_only_reported_supported_levels() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"partial","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":false},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":false}}}},{"id":"disabled","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":false,"low":{"supported":true},"medium":{"supported":true}}}}],"has_more":false,"last_id":"disabled"}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic effort rows decode");
    let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[0].thinking else {
        panic!("first row should have known adaptive thinking");
    };
    assert_eq!(
        accepted.as_slice(),
        &[Effort::Low, Effort::High, Effort::Xhigh]
    );
    let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[1].thinking else {
        panic!("second row should remain known adaptive");
    };
    assert_eq!(accepted.as_slice(), []);
}

#[tokio::test]
async fn anthropic_fetch_paginates_and_atomically_caches_rows() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_anthropic_pages(
        listener,
        [
            r#"{"data":[{"id":"claude-one","max_input_tokens":200000,"max_tokens":8192}],"has_more":true,"last_id":"claude-one"}"#,
            r#"{"data":[{"id":"claude-two","max_input_tokens":300000,"max_tokens":8192}],"has_more":false,"last_id":"claude-two"}"#,
        ],
    );
    let mut provider = provider(
        "anthropic",
        Family::Anthropic,
        &format!("http://127.0.0.1:{port}"),
    );
    provider.auth = AuthStyle::XApiKey;
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };

    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let requests = server.join().expect("serve both model pages");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(fetched.entries.len(), 2);
    assert!(requests[0].contains("limit=1000"));
    assert!(requests[1].contains("after_id=claude-one"));
    let cached = read_cache_async(directory.path().join("models.json"))
        .await
        .expect("read atomically published cache");
    assert_eq!(
        cached
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["claude-one", "claude-two"]
    );
}
#[tokio::test]
async fn failed_live_fetch_uses_atomic_cache_then_typed_default_has_unknown_window() {
    let directory = TestDir::new();
    let port = closed_loopback_port();
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let mut cached = row("openai-codex", "gpt-6-luna", Some(902_500));
    cached.supports_reasoning_summaries = true;
    let cache_path = directory.path().join("models.json");
    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(String::from("token")),
        refresh_token: SecretString::from(String::from("refresh")),
        expires_at: None,
        id_token: None,
        account_id: Some(String::from("account")),
    });
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &oauth,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let other_provider_row = row("anthropic", "claude-sonnet-5", Some(200_000));
    write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![other_provider_row],
    )
    .await
    .expect("write another provider's cache row");
    let other_provider_cache = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(other_provider_cache.source, CatalogSource::Typed);
    assert!(other_provider_cache.entries.is_empty());
    assert!(matches!(
        other_provider_cache.live_error.as_ref(),
        Some(ProviderError::Transport { .. })
    ));

    write_cache_async(cache_path.clone(), "openai-codex".into(), vec![cached])
        .await
        .expect("write Codex cache row");
    let all_cached = read_cache_async(cache_path.clone())
        .await
        .expect("read merged provider cache");
    let cached_providers: Vec<_> = all_cached
        .iter()
        .map(|entry| entry.provider.as_ref())
        .collect();
    assert_eq!(cached_providers.len(), 2);
    assert!(cached_providers.contains(&"anthropic"));
    assert!(cached_providers.contains(&"openai-codex"));
    let cached_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(cached_result.source, CatalogSource::Cache);
    assert_eq!(cached_result.entries[0].provider.as_ref(), "openai-codex");
    assert_eq!(cached_result.entries[0].context_window, Some(902_500));
    assert!(cached_result.entries[0].supports_reasoning_summaries);
    assert!(!cached_result.entries[0].temperature_allowed);
    assert!(!cached_result.entries[0].display_supported);

    fs::remove_file(cache_path).expect("remove cache");
    let typed_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(typed_result.source, CatalogSource::Typed);
    assert!(typed_result.entries.is_empty());
    let catalog =
        Catalog::with_sources(vec![(provider, typed_result.source)], typed_result.entries);
    let typed = resolve(&catalog, &[], "openai-codex/some-new-id")
        .expect("known provider accepts a typed model id");
    assert_eq!(typed.route.id(), "some-new-id");
    assert_eq!(typed.entry.context_window, None);
    assert!(!typed.entry.temperature_allowed);
    assert!(!typed.entry.display_supported);
    assert!(matches!(
        typed.entry.thinking,
        ThinkingSupport::OpenAi {
            none_supported: true,
            ..
        }
    ));
}

#[tokio::test]
async fn version_one_cache_without_optional_capabilities_defaults_false() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    fs::write(
            &cache_path,
            br#"{"version":1,"entries":[{"provider":"anthropic","id":"unknown","display":"unknown","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false,"billing":{"premium_multiplier":null,"price":null}},{"provider":"anthropic","id":"claude-haiku-4-5","display":"Claude Haiku 4.5","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false,"billing":{"premium_multiplier":null,"price":null}}]}"#,
        )
        .expect("write version-one cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read version-one cache");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].image_profile, None);
    assert!(!entries[0].temperature_allowed);
    assert!(entries[1].temperature_allowed);
    assert!(!entries[0].display_supported && !entries[1].display_supported);
    assert_eq!(entries[0].premium_multiplier, None);
    assert_eq!(entries[0].price, None);
    assert_eq!(entries[1].premium_multiplier, None);
    assert_eq!(entries[1].price, None);
}

#[tokio::test]
async fn cached_capability_flags_survive_resolution() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut entry = row("anthropic", "synthetic-capability-model", Some(32_000));
    entry.image_input = true;
    entry.image_profile = Some(ImageProfile::anthropic_standard(32_000));
    entry.temperature_allowed = true;
    entry.display_supported = true;
    write_cache_async(cache_path.clone(), "anthropic".into(), vec![entry])
        .await
        .expect("write capability cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read capability cache");
    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        entries,
    );
    let resolved = resolve(&catalog, &[], "anthropic/synthetic-capability-model")
        .expect("cached model resolves");
    assert_eq!(
        resolved.entry.image_profile,
        Some(ImageProfile::anthropic_standard(32_000))
    );
    assert!(resolved.entry.temperature_allowed && resolved.entry.display_supported);
}

fn serve_anthropic_pages(
    listener: TcpListener,
    pages: [&'static str; 2],
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut request_lines = Vec::with_capacity(pages.len());
        for body in pages {
            let mut stream = accept_loopback(&listener);
            stream
                .set_nonblocking(false)
                .expect("make replay socket blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set replay socket read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone replay socket"));
            let mut request_line = String::new();
            assert_ne!(
                reader
                    .read_line(&mut request_line)
                    .expect("read request line"),
                0
            );
            let mut header_line = String::new();
            loop {
                header_line.clear();
                let bytes = reader
                    .read_line(&mut header_line)
                    .expect("read request header");
                if bytes == 0 || header_line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write replay response");
            request_lines.push(request_line);
        }
        request_lines
    })
}

fn serve_capturing_headers(
    listener: TcpListener,
    body: &'static str,
) -> std::thread::JoinHandle<Vec<(String, String)>> {
    std::thread::spawn(move || {
        let mut stream = accept_loopback(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set capture socket read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone capture socket"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read request line");
        let mut headers = Vec::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line).expect("read request header");
            if bytes == 0 || line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').expect("header has a colon");
            headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
        }
        drop(reader);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write capture response");
        headers
    })
}

async fn fetch_headers(
    id: &str,
    family: Family,
    path: &str,
    auth: AuthStyle,
    credential: &Credential,
    body: &'static str,
) -> Vec<(String, String)> {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind capture listener");
    listener
        .set_nonblocking(true)
        .expect("make capture listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_capturing_headers(listener, body);
    let mut provider = provider(id, family, &format!("http://127.0.0.1:{port}{path}"));
    provider.auth = auth;
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    server.join().expect("capture the model-list request")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

fn oauth(account_id: Option<&str>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(String::from("access")),
        refresh_token: SecretString::from(String::from("refresh")),
        expires_at: None,
        id_token: None,
        account_id: account_id.map(String::from),
    })
}

#[tokio::test]
async fn claude_api_key_model_fetch_sends_x_api_key_and_version() {
    let body = r#"{"data":[],"has_more":false}"#;
    let api_key = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let headers = fetch_headers(
        "anthropic",
        Family::Anthropic,
        "",
        AuthStyle::XApiKey,
        &api_key,
        body,
    )
    .await;
    assert_eq!(header(&headers, "x-api-key"), Some("test-key"));
    assert_eq!(header(&headers, "authorization"), None);
    assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
    assert_eq!(header(&headers, "anthropic-beta"), None);
    assert_eq!(header(&headers, "x-app"), None);
}

#[tokio::test]
async fn claude_oauth_model_fetch_sends_bearer_beta_and_app_markers() {
    let body = r#"{"data":[],"has_more":false}"#;
    let headers = fetch_headers(
        "anthropic",
        Family::Anthropic,
        "",
        AuthStyle::XApiKey,
        &oauth(None),
        body,
    )
    .await;
    assert_eq!(header(&headers, "x-api-key"), None);
    assert_eq!(header(&headers, "authorization"), Some("Bearer access"));
    assert_eq!(header(&headers, "anthropic-version"), Some("2023-06-01"));
    assert_eq!(
        header(&headers, "anthropic-beta"),
        Some("claude-code-20250219,oauth-2025-04-20")
    );
    assert_eq!(header(&headers, "x-app"), Some("cli"));
}

#[tokio::test]
async fn codex_model_fetch_sends_account_and_originator() {
    let headers = fetch_headers(
        "openai-codex",
        Family::Codex,
        "/backend-api/codex",
        AuthStyle::Bearer,
        &oauth(Some("account")),
        r#"{"models":[]}"#,
    )
    .await;
    assert_eq!(header(&headers, "authorization"), Some("Bearer access"));
    assert_eq!(header(&headers, "chatgpt-account-id"), Some("account"));
    assert_eq!(
        header(&headers, "originator"),
        Some(crate::auth::oauth::CODEX_ORIGINATOR)
    );
}

#[tokio::test]
async fn codex_model_fetch_with_an_api_key_is_refused_before_any_request() {
    let directory = TestDir::new();
    let port = closed_loopback_port();
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(matches!(
        &fetched.live_error,
        Some(ProviderError::NoCredentials { provider: id }) if id == "openai-codex"
    ));
}

#[tokio::test]
async fn codex_model_fetch_without_an_account_id_fails_before_any_request() {
    let directory = TestDir::new();
    let port = closed_loopback_port();
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let credential = oauth(None);
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(matches!(
        fetched.live_error,
        Some(ProviderError::NoAccountId)
    ));
}

fn accept_loopback(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // BSD/macOS accepted sockets inherit O_NONBLOCK; request
                // reads below must block.
                stream
                    .set_nonblocking(false)
                    .expect("accepted catalog stream blocking");
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("catalog replay listener failed: {error}"),
        }
    }
}
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create catalog test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn closed_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback port");
    listener.local_addr().expect("read loopback port").port()
}

#[test]
fn exact_priced_model_and_missing_model_have_distinct_costs() {
    use dal_core::Usage;

    let row = PRICE_ROWS
        .iter()
        .find(|row| row.input.is_some() || row.output.is_some())
        .expect("snapshot has a priced row");
    let price = compiled_price(row.model).expect("snapshot model resolves");
    let usage = Usage {
        input_tokens: 1_000_000,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    };
    assert_eq!(usage.cost_usd(None, Some(&price)), Some(price.input));
    assert!(compiled_price("missing/model").is_none());
    assert!(compiled_price(&format!("{}-unlisted", row.model)).is_none());
}
#[test]
fn compiled_price_selects_request_wide_context_tier() {
    use dal_core::Usage;

    let price = compiled_price("deepinfra/Qwen/Qwen3.7-Max").expect("tiered model resolves");
    assert_eq!(price.tiers.len(), 2);
    let at_boundary = Usage {
        input_tokens: 32_000,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    };
    assert_eq!(
        at_boundary.cost_usd(None, Some(&price)),
        Some(32_000.0 * 2.5 / 1_000_000.0)
    );
    let above_boundary = Usage {
        input_tokens: 32_001,
        ..at_boundary
    };
    assert_eq!(
        above_boundary.cost_usd(None, Some(&price)),
        Some(32_001.0 * 5.0 / 1_000_000.0)
    );
    let second_boundary = Usage {
        input_tokens: 128_000,
        ..at_boundary
    };
    assert_eq!(
        second_boundary.cost_usd(None, Some(&price)),
        Some(128_000.0 * 5.0 / 1_000_000.0)
    );
    let above_second_boundary = Usage {
        input_tokens: 128_001,
        ..at_boundary
    };
    assert_eq!(
        above_second_boundary.cost_usd(None, Some(&price)),
        Some(128_001.0 * 6.25 / 1_000_000.0)
    );
}

#[test]
fn tier_table_guard_rejects_unsorted_or_duplicate_models() {
    let unsorted = [
        TierRow {
            model: "b",
            tiers: &[],
        },
        TierRow {
            model: "a",
            tiers: &[],
        },
    ];
    assert!(!tier_rows_are_sorted_unique(&unsorted));

    let duplicate = [
        TierRow {
            model: "model",
            tiers: &[],
        },
        TierRow {
            model: "model",
            tiers: &[],
        },
    ];
    assert!(!tier_rows_are_sorted_unique(&duplicate));
}

#[test]
fn generated_table_remains_sorted_for_binary_search() {
    assert!(
        PRICE_ROWS
            .windows(2)
            .all(|pair| pair[0].model < pair[1].model)
    );
    assert!(
        PRICE_TIER_ROWS
            .windows(2)
            .all(|pair| pair[0].model < pair[1].model)
    );
    assert_eq!(price_source().0, "models.dev");
}

fn devin_config(uid: &str) -> ClientModelConfig {
    ClientModelConfig {
        label: format!("Label {uid}"),
        model_uid: uid.to_owned(),
        max_tokens: 200_000,
        ..Default::default()
    }
}

fn devin_discovery_body(configs: Vec<ClientModelConfig>) -> Vec<u8> {
    GetCliModelConfigsResponse {
        client_model_configs: configs,
        ..Default::default()
    }
    .encode_to_vec()
}

fn serve_binary(
    listener: TcpListener,
    body: Vec<u8>,
    content_type: &'static str,
) -> std::thread::JoinHandle<(String, Vec<(String, String)>)> {
    std::thread::spawn(move || {
        let mut stream = accept_loopback(&listener);
        stream
            .set_nonblocking(false)
            .expect("make replay socket blocking");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set replay socket read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone replay socket"));
        let mut request_line = String::new();
        assert_ne!(
            reader
                .read_line(&mut request_line)
                .expect("read request line"),
            0
        );
        let mut headers = Vec::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).expect("read request header");
            if bytes == 0 || line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').expect("header has a colon");
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().expect("content length parses");
            }
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
        if content_length > 0 {
            let mut discard = vec![0u8; content_length];
            reader.read_exact(&mut discard).expect("read request body");
        }
        drop(reader);
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .expect("write replay head");
        stream.write_all(&body).expect("write replay body");
        (request_line, headers)
    })
}

#[test]
fn devin_discovery_maps_wire_facts_and_never_invents_limits() {
    let provider = provider("my-devin", Family::Devin, "https://server.codeium.com");
    let mut full = devin_config("uid-full");
    full.model_info = Some(ModelInfo {
        max_output_tokens: 4000,
        model_features: Some(ModelFeatures {
            supports_images: true,
            supports_tool_calls: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut zero = devin_config("uid-zero");
    zero.max_tokens = 0;
    let mut disabled = devin_config("uid-disabled");
    disabled.disabled = true;
    let empty = devin_config("   ");
    let mut duplicate = devin_config("uid-full");
    duplicate.label = String::from("Second label wins nothing");
    duplicate.max_tokens = 999;
    let poisoned = devin_config("a\u{7}b");
    let mut body = devin_discovery_body(vec![full, zero, disabled, empty, duplicate, poisoned]);
    body.extend_from_slice(&[0x98, 0x06, 0x01]);
    let rows = decode_devin_models(&provider, &body).expect("devin rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["uid-full", "uid-zero", "a�b"]
    );
    for row in &rows {
        assert_eq!(row.provider.as_ref(), "my-devin");
    }
    assert_eq!(rows[0].display.as_ref(), "Label uid-full");
    assert_eq!(rows[0].context_window, Some(200_000));
    assert_eq!(rows[0].max_output, Some(4000));
    assert!(rows[0].image_input);
    assert_eq!(rows[0].tool_support, ToolSupport::Any);
    assert_eq!(rows[1].context_window, None);
    assert_eq!(rows[1].max_output, None);
    assert!(!rows[1].image_input);
}

#[test]
fn devin_discovery_without_tool_calls_denies_tool_routes() {
    let provider = provider("my-devin", Family::Devin, "https://server.codeium.com");
    let mut config = devin_config("uid-plain");
    config.model_info = Some(ModelInfo {
        model_features: Some(ModelFeatures {
            supports_tool_calls: false,
            ..Default::default()
        }),
        ..Default::default()
    });
    let rows = decode_devin_models(&provider, &devin_discovery_body(vec![config]))
        .expect("devin rows decode");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tool_support, ToolSupport::None);
}

#[test]
fn ollama_tags_prefer_model_ids_dedupe_and_tolerate_unknown_fields() {
    let provider = provider("local-ollama", Family::Ollama, "http://localhost:11434");
    let rows = decode_ollama_models(
        &provider,
        br#"{"models":[{"name":"llama3.1"},{"model":"mistral-small","name":"legacy"},{"name":"llama3.1"},{"unknown":"row"},{"model":"","name":""}],"top_unknown":[1,2]}"#,
    )
    .expect("ollama rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["llama3.1", "mistral-small"]
    );
    for row in &rows {
        assert_eq!(row.provider.as_ref(), "local-ollama");
        assert_eq!(row.context_window, None);
        assert_eq!(row.max_output, None);
    }
    let bs = char::from(92u8);
    let poisoned = format!("{{\"models\":[{{\"name\":\"bad{bs}u0007id\"}}]}}");
    let rows =
        decode_ollama_models(&provider, poisoned.as_bytes()).expect("poisoned ollama row decodes");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.as_ref(), "bad�id");
}

#[tokio::test]
async fn devin_fetch_posts_unary_discovery_without_http_auth() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let mut config = devin_config("devin-test-1");
    config.model_info = Some(ModelInfo {
        max_output_tokens: 8192,
        model_features: Some(ModelFeatures {
            supports_images: true,
            supports_tool_calls: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let server = serve_binary(
        listener,
        devin_discovery_body(vec![config]),
        "application/proto",
    );
    let provider = provider(
        "my-devin",
        Family::Devin,
        &format!("http://127.0.0.1:{port}"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("session-token")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, headers) = server.join().expect("serve the discovery body");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(fetched.entries.len(), 1);
    let entry = &fetched.entries[0];
    assert_eq!(entry.provider.as_ref(), "my-devin");
    assert_eq!(entry.id.as_ref(), "devin-test-1");
    assert_eq!(entry.context_window, Some(200_000));
    assert_eq!(entry.max_output, Some(8192));
    assert!(entry.image_input);
    assert!(
        request_line.starts_with("POST /exa.api_server_pb.ApiServerService/GetCliModelConfigs "),
        "{request_line}"
    );
    assert_eq!(header(&headers, "content-type"), Some("application/proto"));
    assert_eq!(header(&headers, "accept"), Some("*/*"));
    assert_eq!(header(&headers, "connect-protocol-version"), Some("1"));
    assert_eq!(header(&headers, "authorization"), None);
}

#[tokio::test]
async fn devin_fetch_without_an_api_key_is_refused_before_any_request() {
    let directory = TestDir::new();
    let port = closed_loopback_port();
    let provider = provider(
        "my-devin",
        Family::Devin,
        &format!("http://127.0.0.1:{port}"),
    );
    let client = reqwest::Client::new();
    for credential in [Credential::None, oauth(None)] {
        let fetch = ModelFetch {
            client: &client,
            provider: &provider,
            credential: &credential,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
        };
        let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(fetched.source, CatalogSource::Typed);
        assert!(
            matches!(&fetched.live_error, Some(ProviderError::NoCredentials { provider }) if provider == "my-devin"),
            "{:?}",
            fetched.live_error
        );
    }
}

#[tokio::test]
async fn devin_fetch_with_a_malformed_body_falls_back_typed() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, vec![0xFF, 0xFE, 0xFD], "application/proto");
    let provider = provider(
        "my-devin",
        Family::Devin,
        &format!("http://127.0.0.1:{port}"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("session-token")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    server.join().expect("serve the malformed body");
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::Devin,
                ..
            })
        ),
        "{:?}",
        fetched.live_error
    );
}

#[tokio::test]
async fn ollama_fetch_trims_api_suffix_and_lists_tags_without_auth() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let body = br#"{"models":[{"name":"llama3.1"},{"model":"mistral-small","name":"legacy"},{"name":""}],"top_unknown":[1]}"#.to_vec();
    let server = serve_binary(listener, body, "application/json");
    let mut provider = provider(
        "local-ollama",
        Family::Ollama,
        &format!("http://127.0.0.1:{port}/api/"),
    );
    provider.auth = AuthStyle::None;
    let credential = Credential::None;
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, headers) = server.join().expect("serve the tags body");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["llama3.1", "mistral-small"]
    );
    for entry in &fetched.entries {
        assert_eq!(entry.provider.as_ref(), "local-ollama");
    }
    assert!(request_line.starts_with("GET /api/tags "), "{request_line}");
    assert_eq!(header(&headers, "authorization"), None);
    assert_eq!(header(&headers, "x-api-key"), None);
    assert_eq!(header(&headers, "api-key"), None);
}

#[tokio::test]
async fn ollama_fetch_with_a_malformed_body_falls_back_typed() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, b"not json".to_vec(), "application/json");
    let mut provider = provider(
        "local-ollama",
        Family::Ollama,
        &format!("http://127.0.0.1:{port}"),
    );
    provider.auth = AuthStyle::None;
    let credential = Credential::None;
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, headers) = server.join().expect("serve the malformed body");
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::Ollama,
                ..
            })
        ),
        "{:?}",
        fetched.live_error
    );
    assert!(request_line.starts_with("GET /api/tags "), "{request_line}");
    assert_eq!(header(&headers, "authorization"), None);
}

#[test]
fn mistral_cards_map_context_vision_and_tools_without_invented_fields() {
    let provider = provider("mistral", Family::Mistral, "https://api.mistral.ai/v1");
    let rows = decode_mistral_models(
        &provider,
        br#"{"object":"list","data":[{"id":"mistral-large-latest","capabilities":{"completion_chat":true,"function_calling":true,"vision":true,"future_flag":true},"max_context_length":128000},{"id":"mistral-embed","capabilities":{"completion_chat":false,"function_calling":false},"max_context_length":32768},{"id":"retired","archived":true,"max_context_length":100},{"id":"","max_context_length":50}],"unknown":"x"}"#,
    )
    .expect("mistral rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["mistral-large-latest", "mistral-embed"]
    );
    for row in &rows {
        assert_eq!(row.provider.as_ref(), "mistral");
    }
    assert_eq!(rows[0].display.as_ref(), "mistral-large-latest");
    assert_eq!(rows[0].context_window, Some(128_000));
    assert!(rows[0].image_input);
    assert_eq!(rows[0].tool_support, ToolSupport::Any);
    assert_eq!(rows[1].context_window, Some(32_768));
    assert!(!rows[1].image_input);
    assert_eq!(rows[1].tool_support, ToolSupport::None);
}

#[test]
fn mistral_cards_also_decode_from_a_bare_array() {
    let provider = provider("mistral", Family::Mistral, "https://api.mistral.ai/v1");
    let rows = decode_mistral_models(
        &provider,
        br#"[{"id":"open-mistral-7b","max_context_length":32768}]"#,
    )
    .expect("mistral rows decode");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.as_ref(), "open-mistral-7b");
    assert_eq!(rows[0].context_window, Some(32_768));
    assert_eq!(rows[0].max_output, None);
    assert!(!rows[0].image_input);
    assert_eq!(rows[0].tool_support, ToolSupport::Any);
}

#[tokio::test]
async fn mistral_fetch_lists_models_with_bearer_auth() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let body = br#"{"object":"list","data":[{"id":"mistral-large-latest","capabilities":{"completion_chat":true,"function_calling":true,"vision":false},"max_context_length":131072}]}"#.to_vec();
    let server = serve_binary(listener, body, "application/json");
    let provider = provider(
        "mistral",
        Family::Mistral,
        &format!("http://127.0.0.1:{port}/v1"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, headers) = server.join().expect("serve the mistral body");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(fetched.entries.len(), 1);
    let entry = &fetched.entries[0];
    assert_eq!(entry.provider.as_ref(), "mistral");
    assert_eq!(entry.id.as_ref(), "mistral-large-latest");
    assert_eq!(entry.context_window, Some(131_072));
    assert!(
        request_line.starts_with("GET /v1/models "),
        "{request_line}"
    );
    assert_eq!(header(&headers, "authorization"), Some("Bearer test-key"));
}

#[tokio::test]
async fn mistral_fetch_with_a_malformed_body_falls_back_typed() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, b"not json".to_vec(), "application/json");
    let provider = provider(
        "mistral",
        Family::Mistral,
        &format!("http://127.0.0.1:{port}/v1"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    server.join().expect("serve the malformed body");
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::Mistral,
                ..
            })
        ),
        "{:?}",
        fetched.live_error
    );
}

fn radius_provider(base_url: &str) -> ProviderEntry {
    provider("radius", Family::PiMessages, base_url)
}

const RADIUS_CONFIG: &[u8] = br#"{"baseUrl":"https://radius.pi.dev/v1","unknown":1,"models":[
    {"id":"balanced","name":"Balanced","reasoning":true,
     "thinkingLevelMap":{"off":null,"minimal":null,"low":"low","medium":null,"high":"high","xhigh":null,"max":"max"},
     "input":["text","image"],"contextWindow":1048576,"maxTokens":131072,
     "cost":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":0}},
    {"id":"claude-fable-5","name":"Claude Fable 5","reasoning":true,
     "thinkingLevelMap":{"off":null,"xhigh":"xhigh","max":"max"},
     "input":["text","image"],"contextWindow":1000000,"maxTokens":128000,
     "cost":{"input":10,"output":50,"cacheRead":1,"cacheWrite":12.5}},
    {"id":"cheap","name":"Cheap","reasoning":false,"input":["text"],
     "contextWindow":32768,"maxTokens":8192,"cost":{"input":0.3,"output":1.2,"cacheRead":0.006,"cacheWrite":0}},
    {"id":"cheap","name":"Duplicate","reasoning":false,"input":["text"],
     "contextWindow":1,"maxTokens":1,"cost":{}},
    {"id":"no-window","name":"No window","reasoning":false,"input":["text"],"maxTokens":1,"cost":{}},
    {"id":"bad-reasoning","name":"Bad","reasoning":"yes","input":["text"],"contextWindow":1,"maxTokens":1,"cost":{}},
    {"id":"cost-list","name":"Bad cost","reasoning":false,"input":["text"],"contextWindow":1,"maxTokens":1,"cost":[]},
    "not an object"
]}"#;

#[test]
fn radius_config_maps_limits_thinking_and_price_and_drops_invalid_rows() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let rows = decode_radius_models(&provider, RADIUS_CONFIG).expect("radius rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["balanced", "claude-fable-5", "cheap"],
        "invalid and duplicate rows are dropped, not errors"
    );
    for row in &rows {
        assert_eq!(row.provider.as_ref(), "radius");
        assert_eq!(row.listing, Listing::Listed);
    }
    let balanced = &rows[0];
    assert_eq!(balanced.display.as_ref(), "Balanced");
    assert_eq!(balanced.context_window, Some(1_048_576));
    assert_eq!(balanced.max_output, Some(131_072));
    assert!(balanced.image_input);
    assert_eq!(balanced.tool_support, ToolSupport::Any);
    let price = balanced.price.as_ref().expect("a flat price");
    assert_eq!(
        (
            price.input,
            price.cached_input,
            price.output,
            price.reasoning
        ),
        (3.0, 0.3, 15.0, 15.0)
    );
    assert_eq!(price.cache_write, Some(0.0));
    assert!(price.tiers.is_empty());
    // A null level is unsupported; the others up to `high` are supported, and
    // `xhigh` and `max` only when the map names them.
    match &balanced.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => {
            assert_eq!(
                accepted,
                &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
            );
            assert!(!none_supported);
        }
        other => panic!("{other:?}"),
    }
    match &rows[1].thinking {
        ThinkingSupport::OpenAi { accepted, .. } => assert_eq!(
            accepted,
            &[
                ThinkingLevel::Minimal,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::Xhigh,
                ThinkingLevel::Max,
            ]
        ),
        other => panic!("{other:?}"),
    }
    let cheap = &rows[2];
    assert!(!cheap.image_input);
    match &cheap.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => assert!(accepted.is_empty() && *none_supported),
        other => panic!("{other:?}"),
    }
}

#[test]
fn radius_config_recovers_a_valid_duplicate_after_a_dropped_row() {
    // A row dropped at the `cost` gate must not mark its id as seen: a later
    // valid row reusing the id still decodes.
    let provider = radius_provider("https://radius.pi.dev/v1");
    let body = br#"{"baseUrl":"https://radius.pi.dev/v1","models":[
        {"id":"dup","name":"Dropped","reasoning":false,"input":["text"],
         "contextWindow":1,"maxTokens":1,"cost":[]},
        {"id":"dup","name":"Kept","reasoning":false,"input":["text"],
         "contextWindow":4096,"maxTokens":512,
         "cost":{"input":1,"output":2,"cacheRead":0.1,"cacheWrite":0}}
    ]}"#;
    let rows = decode_radius_models(&provider, body).expect("radius rows decode");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id.as_ref(), "dup");
    assert_eq!(rows[0].display.as_ref(), "Kept");
    assert_eq!(rows[0].price.as_ref().map(|price| price.input), Some(1.0));
}

#[test]
fn radius_config_without_base_url_or_a_models_array_is_a_protocol_error() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    for body in [
        &br#"{"models":[]}"#[..],
        br#"{"baseUrl":"https://radius.pi.dev/v1"}"#,
        br#"{"baseUrl":"https://radius.pi.dev/v1","models":{}}"#,
        br#"{"baseUrl":7,"models":[]}"#,
        b"not json",
    ] {
        assert!(
            matches!(
                decode_radius_models(&provider, body),
                Err(ProviderError::Protocol {
                    family: Family::PiMessages,
                    ..
                })
            ),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn radius_models_resolve_qualified_and_by_route_without_special_casing() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let rows = decode_radius_models(&provider, RADIUS_CONFIG).expect("radius rows decode");
    let listed = Catalog::with_sources(vec![(provider.clone(), CatalogSource::Live)], rows);
    let route = ModelRoute::Api {
        family: Family::PiMessages,
        model: "balanced".into(),
    };
    let resolved = resolve(&listed, &[], "radius/balanced").expect("a listed row resolves");
    assert_eq!(resolved.provider.as_ref(), "radius");
    assert_eq!(resolved.route, route);
    assert_eq!(resolved.entry.context_window, Some(1_048_576));
    assert_eq!(
        resolve_route(&listed, &[], &route)
            .expect("the route resolves")
            .route,
        route
    );
    assert_eq!(
        resolve(&listed, &[], "radius/unlisted")
            .unwrap_err()
            .to_string(),
        "unknown model radius/unlisted",
        "a listed catalog rejects an unlisted id"
    );

    let typed = Catalog::with_sources(vec![(provider, CatalogSource::Typed)], Vec::new());
    let unlisted = ModelRoute::Api {
        family: Family::PiMessages,
        model: "claude-opus-5".into(),
    };
    let resolved = resolve(&typed, &[], "radius/claude-opus-5").expect("a typed id resolves");
    assert_eq!(resolved.route, unlisted);
    assert_eq!(resolved.entry.context_window, None);
    let by_route = resolve_route(&typed, &[], &unlisted).expect("the family prefix is radius");
    assert_eq!(by_route.provider.as_ref(), "radius");
}

#[tokio::test]
async fn radius_fetch_reads_the_gateway_config_at_the_origin_with_bearer_auth() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, RADIUS_CONFIG.to_vec(), "application/json");
    // A path on the base never prefixes the config request.
    let provider = radius_provider(&format!("http://127.0.0.1:{port}/gateway/v1"));
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("radius-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, headers) = server.join().expect("serve the radius config");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["balanced", "claude-fable-5", "cheap"]
    );
    assert!(
        request_line.starts_with("GET /v1/config "),
        "{request_line}"
    );
    assert_eq!(header(&headers, "authorization"), Some("Bearer radius-key"));
    let cached = read_cache_async(directory.path().join("models.json"))
        .await
        .expect("the live rows are cached");
    assert_eq!(cached.len(), 3);
}

#[tokio::test]
async fn radius_fetch_with_a_malformed_body_falls_back_typed() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, br#"{"models":[]}"#.to_vec(), "application/json");
    let provider = radius_provider(&format!("http://127.0.0.1:{port}/v1"));
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("radius-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    server.join().expect("serve the malformed config");
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::PiMessages,
                ..
            })
        ),
        "{:?}",
        fetched.live_error
    );
}

fn copilot_fixture() -> &'static [u8] {
    br#"{"data":[
        {"id":"gpt-4.1","name":"GPT-4.1","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384},"supports":{"tool_calls":true,"vision":true}}},
        {"id":"claude-haiku-4.5","name":"Claude Haiku 4.5","capabilities":{"type":"chat","limits":{"max_context_window_tokens":200000,"max_output_tokens":8000},"supports":{"tool_calls":false,"vision":false}}},
        {"id":"text-embedding-3-small","capabilities":{"type":"embeddings"}},
        {"id":42}
    ]}"#
}

#[test]
fn copilot_models_map_only_the_provider_surface() {
    let chat = provider(
        "github-copilot",
        Family::Chat,
        "https://api.githubcopilot.com",
    );
    let rows = decode_copilot_models(&chat, copilot_fixture()).expect("copilot rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["gpt-4.1"]
    );
    assert_eq!(rows[0].display.as_ref(), "GPT-4.1");
    assert_eq!(rows[0].provider.as_ref(), "github-copilot");
    assert_eq!(rows[0].context_window, Some(128_000));
    assert_eq!(rows[0].max_output, Some(16_384));
    assert!(rows[0].image_input);
    assert_eq!(rows[0].tool_support, ToolSupport::Any);

    let anthropic = provider(
        "github-copilot-anthropic",
        Family::Anthropic,
        "https://api.githubcopilot.com",
    );
    let rows = decode_copilot_models(&anthropic, copilot_fixture()).expect("copilot rows decode");
    assert_eq!(
        rows.iter().map(|row| row.id.as_ref()).collect::<Vec<_>>(),
        ["claude-haiku-4.5"]
    );
    assert_eq!(rows[0].context_window, Some(200_000));
    assert_eq!(rows[0].max_output, Some(8000));
    assert!(!rows[0].image_input);
    assert_eq!(rows[0].tool_support, ToolSupport::None);
}

#[tokio::test]
async fn copilot_fetch_lists_account_models_with_cli_identity() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, copilot_fixture().to_vec(), "application/json");
    let provider = provider(
        "github-copilot",
        Family::Chat,
        &format!("http://127.0.0.1:{port}"),
    );
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("gho_tok")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["gpt-4.1"]
    );
    assert_eq!(fetched.entries[0].provider.as_ref(), "github-copilot");
    assert_eq!(fetched.entries[0].context_window, Some(128_000));
    let (request_line, headers) = server.join().expect("serve the copilot body");
    assert!(request_line.starts_with("GET /models "), "{request_line}");
    assert_eq!(header(&headers, "authorization"), Some("Bearer gho_tok"));
    assert!(header(&headers, "editor-version").is_some());
}

#[test]
fn bedrock_typed_carries_anthropic_contract() {
    let made = provider(
        "my-bedrock",
        Family::Bedrock,
        "https://bedrock-runtime.us-east-1.amazonaws.com",
    );
    let catalog = Catalog::with_sources(vec![(made, CatalogSource::Typed)], Vec::new());
    let resolved = resolve(&catalog, &[], "my-bedrock/anthropic.claude-sonnet-4-5:0")
        .expect("bedrock typed id resolves");
    assert!(matches!(
        resolved.entry.thinking,
        ThinkingSupport::UnknownAdaptive
    ));
    let limits = crate::thinking::RequestLimits {
        max_tokens: 0,
        max_output: 2048,
        temperature_allowed: false,
    };
    let plan = crate::thinking::plan(
        dal_core::ThinkingLevel::Off,
        &BeforeRequestPatch::default(),
        &resolved.entry.thinking,
        limits,
    );
    assert!(matches!(
        plan.wire,
        crate::thinking::WireThinking::Anthropic { .. }
    ));
    let request = dal_core::ModelRequest {
        purpose: dal_core::Purpose::Turn,
        model: resolved.route,
        system: "".into(),
        tools: Vec::new().into(),
        context: Vec::new().into(),
        params: dal_core::RequestParams {
            thinking: dal_core::ThinkingLevel::Off,
            effort: None,
            temperature: None,
            max_output_tokens: None,
        },
        cache_key: None,
    };
    let body = crate::family::bedrock::request_body(&request, &plan);
    assert!(body.is_ok(), "converse accepts catalog thinking: {body:?}");
}

#[test]
fn gitlab_typed_thinking_follows_proxy_route() {
    let made = provider("gitlab-duo", Family::GitLab, "https://gitlab.com");
    let catalog = Catalog::with_sources(vec![(made, CatalogSource::Typed)], Vec::new());
    let anthropic =
        resolve(&catalog, &[], "gitlab-duo/duo-chat-sonnet-4-5").expect("anthropic route resolves");
    assert!(matches!(
        anthropic.entry.thinking,
        ThinkingSupport::UnknownAdaptive
    ));
    let responses = resolve(&catalog, &[], "gitlab-duo/duo-chat-gpt-5-codex")
        .expect("responses route resolves");
    assert!(matches!(
        responses.entry.thinking,
        ThinkingSupport::OpenAi { .. }
    ));
    let completions = resolve(&catalog, &[], "gitlab-duo/duo-chat-gpt-5-mini")
        .expect("completions route resolves");
    assert!(matches!(
        completions.entry.thinking,
        ThinkingSupport::OpenAi { .. }
    ));
    assert_eq!(
        crate::family::gitlab::proxy_route("duo-chat-sonnet-4-5"),
        crate::family::gitlab::ProxyRoute::Anthropic
    );
    assert_eq!(
        crate::family::gitlab::proxy_route("duo-chat-gpt-5-codex"),
        crate::family::gitlab::ProxyRoute::Responses
    );
    assert_eq!(
        crate::family::gitlab::proxy_route("duo-chat-gpt-5-mini"),
        crate::family::gitlab::ProxyRoute::Completions
    );
    let limits = crate::thinking::RequestLimits {
        max_tokens: 0,
        max_output: 0,
        temperature_allowed: false,
    };
    let anthropic_plan = crate::thinking::plan(
        dal_core::ThinkingLevel::Off,
        &BeforeRequestPatch::default(),
        &anthropic.entry.thinking,
        limits,
    );
    assert!(matches!(
        anthropic_plan.wire,
        crate::thinking::WireThinking::Anthropic { .. }
    ));
    let responses_plan = crate::thinking::plan(
        dal_core::ThinkingLevel::Off,
        &BeforeRequestPatch::default(),
        &responses.entry.thinking,
        limits,
    );
    assert!(matches!(
        responses_plan.wire,
        crate::thinking::WireThinking::OpenAi { .. }
    ));
}

#[test]
fn gemini_cli_typed_carries_no_tools() {
    let made = provider("my-gemini", Family::GoogleGeminiCli, "http://localhost");
    let catalog = Catalog::with_sources(vec![(made, CatalogSource::Typed)], Vec::new());
    let resolved =
        resolve(&catalog, &[], "my-gemini/gemini-2.5-pro").expect("gemini cli typed resolves");
    assert_eq!(resolved.entry.tool_support, ToolSupport::None);
}

#[tokio::test]
async fn gemini_cli_has_no_live_discovery() {
    let directory = TestDir::new();
    let made = provider("my-gemini", Family::GoogleGeminiCli, "http://127.0.0.1:1");
    let credential = Credential::None;
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &made,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
}

/// One request the scripted Gemini server answered: the request line and the
/// lowercased request headers.
type SeenRequest = (String, Vec<(String, String)>);

/// Answers one request per `replies` entry, in order, each as
/// `(status, body)`, and returns every request it saw. A client that asks for
/// more pages than scripted finds the listener closed.
fn serve_replies(
    listener: TcpListener,
    replies: Vec<(u16, Vec<u8>)>,
) -> std::thread::JoinHandle<Vec<SeenRequest>> {
    std::thread::spawn(move || {
        let mut seen = Vec::with_capacity(replies.len());
        for (status, body) in replies {
            let mut stream = accept_loopback(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set replay socket read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone replay socket"));
            let mut request_line = String::new();
            assert_ne!(
                reader
                    .read_line(&mut request_line)
                    .expect("read request line"),
                0
            );
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                let bytes = reader.read_line(&mut line).expect("read request header");
                if bytes == 0 || line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').expect("header has a colon");
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
            }
            drop(reader);
            let head = format!(
                "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(head.as_bytes())
                .expect("write replay head");
            stream.write_all(&body).expect("write replay body");
            seen.push((request_line, headers));
        }
        seen
    })
}

fn gemini_key(key: &str) -> Credential {
    Credential::ApiKey {
        key: SecretString::from(String::from(key)),
    }
}

fn gemini_provider(base_url: &str) -> ProviderEntry {
    provider("google", Family::GoogleGenerativeAi, base_url)
}

/// One refresh of a direct-Gemini provider against `replies`, with every
/// request the server saw. The base carries a trailing slash on purpose: the
/// list URL must not double it.
async fn gemini_refresh(
    credential: &Credential,
    replies: Vec<(u16, Vec<u8>)>,
) -> (CatalogFetch, Vec<SeenRequest>) {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_replies(listener, replies);
    let provider = gemini_provider(&format!("http://127.0.0.1:{port}/v1beta/"));
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    (fetched, server.join().expect("serve the gemini replies"))
}

fn gemini_card(name: &str, display: &str, input: u32, output: u32) -> String {
    format!(
        r#"{{"name":"{name}","displayName":"{display}","supportedGenerationMethods":["generateContent","countTokens"],"inputTokenLimit":{input},"outputTokenLimit":{output}}}"#
    )
}

fn gemini_page(cards: &[String], next: Option<&str>) -> (u16, Vec<u8>) {
    let token = next.map_or_else(String::new, |token| {
        format!(r#","nextPageToken":"{token}""#)
    });
    (
        200,
        format!(r#"{{"models":[{}]{token}}}"#, cards.join(",")).into_bytes(),
    )
}

fn gemini_ids(fetched: &CatalogFetch) -> Vec<&str> {
    fetched
        .entries
        .iter()
        .map(|entry| entry.id.as_ref())
        .collect()
}

#[tokio::test]
async fn gemini_fetch_pages_with_the_key_header_and_never_a_key_in_the_url() {
    let first = gemini_page(
        &[
            gemini_card("models/gemini-2.5-pro", "Gemini 2.5 Pro", 1_048_576, 65_536),
            gemini_card("models/gemini-2.5-flash", "Gemini 2.5 Flash", 1_048_576, 65_536),
        ],
        Some("page-2"),
    );
    let second = gemini_page(
        &[
            gemini_card("models/gemini-1.5-flash", "Gemini 1.5 Flash", 1_000_000, 8_192),
            gemini_card("models/gemini-2.5-pro", "Gemini 2.5 Pro again", 1_048_576, 65_536),
        ],
        None,
    );
    let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![first, second]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        gemini_ids(&fetched),
        ["gemini-1.5-flash", "gemini-2.5-flash", "gemini-2.5-pro"],
        "rows are deduplicated across pages and sorted by id"
    );
    for entry in &fetched.entries {
        assert_eq!(entry.provider.as_ref(), "google");
    }
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[0].0,
        "GET /v1beta/models?pageSize=100 HTTP/1.1\r\n"
    );
    assert_eq!(
        seen[1].0,
        "GET /v1beta/models?pageSize=100&pageToken=page-2 HTTP/1.1\r\n"
    );
    for (request_line, headers) in &seen {
        assert_eq!(header(headers, "x-goog-api-key"), Some("test-key"));
        assert_eq!(header(headers, "authorization"), None);
        assert!(
            !request_line.contains("test-key") && !request_line.contains("key="),
            "the key must never travel in the URL: {request_line}"
        );
    }
}

#[tokio::test]
async fn gemini_fetch_keeps_generate_content_rows_and_maps_limits() {
    let cards = [
        gemini_card("models/gemini-2.5-pro", "Gemini 2.5 Pro", 1_048_576, 65_536),
        // Not text generation: an embedding, an image-generation, and an
        // answer-only model.
        r#"{"name":"models/text-embedding-004","displayName":"Embedding","supportedGenerationMethods":["embedContent"],"inputTokenLimit":2048}"#.to_owned(),
        r#"{"name":"models/imagen-3.0-generate","supportedGenerationMethods":["predict"]}"#.to_owned(),
        r#"{"name":"models/aqa","supportedGenerationMethods":["generateAnswer"]}"#.to_owned(),
        r#"{"name":"models/no-methods"}"#.to_owned(),
        // A bare name keeps its id; non-positive limits are unknown, never
        // zero; a missing display name is the id; no image hint in the id.
        r#"{"name":"gemma-3-27b-it","supportedGenerationMethods":["generateContent"],"inputTokenLimit":0,"outputTokenLimit":-5}"#.to_owned(),
        // A blank display name is the id; a limit of the wrong type is unknown.
        r#"{"name":"models/Gemini-Custom","displayName":"   ","supportedGenerationMethods":["generateContent"],"inputTokenLimit":"big","outputTokenLimit":null}"#.to_owned(),
        r#"{"name":"models/learnlm-2.0-flash","supportedGenerationMethods":["generateContent"],"inputTokenLimit":1048576}"#.to_owned(),
        // Terminal control and bidi characters never reach an id or a name.
        "{\"name\":\"models/vision-model\\u0007x\",\"displayName\":\"Pretty\\u202eName\",\"supportedGenerationMethods\":[\"generateContent\"]}".to_owned(),
        // Malformed rows are skipped, not fatal.
        r#"{"name":42,"supportedGenerationMethods":["generateContent"]}"#.to_owned(),
        r#"{"displayName":"No Name","supportedGenerationMethods":["generateContent"]}"#.to_owned(),
        r#"{"name":"models/","supportedGenerationMethods":["generateContent"]}"#.to_owned(),
        r#"{"name":"models/bad-methods","supportedGenerationMethods":"generateContent"}"#.to_owned(),
        r#"{"name":"models/bad-method-type","supportedGenerationMethods":[1,2]}"#.to_owned(),
        r#"["models/array-row"]"#.to_owned(),
        r#""models/string-row""#.to_owned(),
        "7".to_owned(),
        "null".to_owned(),
    ];
    let (fetched, seen) = gemini_refresh(
        &gemini_key("test-key"),
        vec![gemini_page(&cards, None)],
    )
    .await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(seen.len(), 1);
    assert_eq!(
        gemini_ids(&fetched),
        [
            "Gemini-Custom",
            "gemini-2.5-pro",
            "gemma-3-27b-it",
            "learnlm-2.0-flash",
            "vision-model\u{FFFD}x",
        ]
    );
    let pro = &fetched.entries[1];
    assert_eq!(pro.display.as_ref(), "Gemini 2.5 Pro");
    assert_eq!(pro.context_window, Some(1_048_576));
    assert_eq!(pro.max_output, Some(65_536));
    assert!(pro.image_input);
    assert_eq!(pro.tool_support, ToolSupport::Any);
    assert_eq!(pro.price, None);

    let custom = &fetched.entries[0];
    assert_eq!(custom.display.as_ref(), "Gemini-Custom");
    assert_eq!(custom.context_window, None);
    assert_eq!(custom.max_output, None);
    assert!(custom.image_input, "a gemini id accepts images, any case");

    let gemma = &fetched.entries[2];
    assert_eq!(gemma.display.as_ref(), "gemma-3-27b-it");
    assert_eq!(gemma.context_window, None);
    assert_eq!(gemma.max_output, None);
    assert!(!gemma.image_input);

    let learnlm = &fetched.entries[3];
    assert_eq!(learnlm.context_window, Some(1_048_576));
    assert_eq!(learnlm.max_output, None);
    assert!(!learnlm.image_input);

    let vision = &fetched.entries[4];
    assert_eq!(vision.display.as_ref(), "Pretty\u{FFFD}Name");
    assert!(vision.image_input);
}

#[tokio::test]
async fn gemini_discovered_rows_agree_with_typed_rows_on_thinking_and_tools() {
    let (fetched, _) = gemini_refresh(
        &gemini_key("test-key"),
        vec![gemini_page(
            &[gemini_card("models/gemini-2.5-pro", "Gemini 2.5 Pro", 1_048_576, 65_536)],
            None,
        )],
    )
    .await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    let provider = gemini_provider("https://example.test/v1beta");
    let typed = resolve(
        &Catalog::with_sources(vec![(provider, CatalogSource::Typed)], Vec::new()),
        &[],
        "google/gemini-2.5-pro",
    )
    .expect("a typed gemini id resolves");
    let live = &fetched.entries[0];
    let ladder = |support: &ThinkingSupport| match support {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => (accepted.clone(), *none_supported),
        other => panic!("gemini rows carry {other:?}"),
    };
    assert_eq!(ladder(&live.thinking), ladder(&typed.entry.thinking));
    assert_eq!(live.tool_support, typed.entry.tool_support);
    assert_eq!(live.temperature_allowed, typed.entry.temperature_allowed);
}

#[tokio::test]
async fn gemini_fetch_stops_on_a_repeated_page_token() {
    let first = gemini_page(
        &[gemini_card("models/gemini-a", "A", 10, 5)],
        Some("again"),
    );
    let second = gemini_page(
        &[gemini_card("models/gemini-b", "B", 10, 5)],
        Some("again"),
    );
    let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![first, second]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(gemini_ids(&fetched), ["gemini-a", "gemini-b"]);
    assert_eq!(seen.len(), 2, "the repeated token is not followed");
    assert_eq!(
        seen[1].0,
        "GET /v1beta/models?pageSize=100&pageToken=again HTTP/1.1\r\n"
    );
}

#[tokio::test]
async fn gemini_fetch_stops_on_a_blank_or_non_string_page_token() {
    for token in [r#""""#, r#""   ""#, "7", "null", "[]"] {
        let body = format!(
            r#"{{"models":[{}],"nextPageToken":{token}}}"#,
            gemini_card("models/gemini-a", "A", 10, 5)
        )
        .into_bytes();
        let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![(200, body)]).await;
        assert_eq!(
            fetched.source,
            CatalogSource::Live,
            "token {token}: {:?}",
            fetched.live_error
        );
        assert_eq!(gemini_ids(&fetched), ["gemini-a"], "token {token}");
        assert_eq!(seen.len(), 1, "token {token} must end pagination");
    }
}

#[tokio::test]
async fn gemini_fetch_reads_at_most_twenty_five_pages() {
    let replies: Vec<(u16, Vec<u8>)> = (0..25)
        .map(|page| {
            gemini_page(
                &[gemini_card(&format!("models/gemini-{page:02}"), "Page", 10, 5)],
                Some(&format!("t{page}")),
            )
        })
        .collect();
    let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), replies).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(seen.len(), 25, "a 26th page is never requested");
    assert_eq!(fetched.entries.len(), 25);
    assert_eq!(
        seen[24].0,
        "GET /v1beta/models?pageSize=100&pageToken=t23 HTTP/1.1\r\n"
    );
}

#[tokio::test]
async fn gemini_fetch_with_a_malformed_body_falls_back_typed() {
    let bodies: [&[u8]; 5] = [
        b"<html><body>Bad gateway</body></html>",
        b"not json",
        b"[]",
        br#"{"models":"nope"}"#,
        br#"{"models":null}"#,
    ];
    for body in bodies {
        let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![(200, body.to_vec())]).await;
        assert_eq!(fetched.source, CatalogSource::Typed, "{body:?}");
        assert!(fetched.entries.is_empty());
        assert!(
            matches!(
                &fetched.live_error,
                Some(ProviderError::Protocol {
                    family: Family::GoogleGenerativeAi,
                    ..
                })
            ),
            "{body:?}: {:?}",
            fetched.live_error
        );
        assert_eq!(seen.len(), 1);
    }
}

#[tokio::test]
async fn gemini_fetch_with_a_bad_later_page_returns_no_partial_list() {
    let first = gemini_page(
        &[gemini_card("models/gemini-a", "A", 10, 5)],
        Some("page-2"),
    );
    let (fetched, seen) = gemini_refresh(
        &gemini_key("test-key"),
        vec![first, (200, b"<html>nope</html>".to_vec())],
    )
    .await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::GoogleGenerativeAi,
                ..
            })
        ),
        "{:?}",
        fetched.live_error
    );
    assert_eq!(seen.len(), 2);
}

#[tokio::test]
async fn gemini_fetch_with_a_rejected_status_falls_back_typed() {
    for status in [401_u16, 403, 429, 503] {
        let (fetched, seen) = gemini_refresh(
            &gemini_key("test-key"),
            vec![(status, br#"{"error":{"code":0,"message":"no"}}"#.to_vec())],
        )
        .await;
        assert_eq!(fetched.source, CatalogSource::Typed, "{status}");
        assert!(
            matches!(
                &fetched.live_error,
                Some(ProviderError::Status {
                    family: Family::GoogleGenerativeAi,
                    status: found,
                    ..
                }) if *found == status
            ),
            "{status}: {:?}",
            fetched.live_error
        );
        assert_eq!(seen.len(), 1, "{status}");
    }
}

#[tokio::test]
async fn gemini_fetch_without_a_usable_key_is_refused_before_any_request() {
    let port = closed_loopback_port();
    let provider = gemini_provider(&format!("http://127.0.0.1:{port}/v1beta"));
    let client = reqwest::Client::new();
    for credential in [Credential::None, gemini_key(""), gemini_key("  \t")] {
        let directory = TestDir::new();
        let fetch = ModelFetch {
            client: &client,
            provider: &provider,
            credential: &credential,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
        };
        let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(fetched.source, CatalogSource::Typed);
        assert!(fetched.entries.is_empty());
        assert!(
            matches!(
                &fetched.live_error,
                Some(ProviderError::NoCredentials { provider }) if provider == "google"
            ),
            "{:?}",
            fetched.live_error
        );
    }
    let directory = TestDir::new();
    let oauth = oauth(None);
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &oauth,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::InvalidRequest { message }) if message.contains("OAuth")
        ),
        "{:?}",
        fetched.live_error
    );
}

#[tokio::test]
async fn vertex_and_antigravity_list_no_models_and_say_why_while_typed_ids_resolve() {
    let cases = [
        (Family::GoogleVertex, "my-vertex", "publisher models"),
        (Family::GoogleAntigravity, "my-antigravity", "OAuth"),
        (Family::GoogleGeminiCli, "my-gemini", "gemini CLI"),
    ];
    for (family, id, reason) in cases {
        let directory = TestDir::new();
        let made = provider(id, family, "http://127.0.0.1:1");
        let credential = Credential::None;
        let client = reqwest::Client::new();
        let fetch = ModelFetch {
            client: &client,
            provider: &made,
            credential: &credential,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
        };
        let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
        assert_eq!(fetched.source, CatalogSource::Typed, "{family:?}");
        assert!(fetched.entries.is_empty(), "{family:?}");
        let Some(ProviderError::InvalidRequest { message }) = &fetched.live_error else {
            panic!("{family:?}: {:?}", fetched.live_error);
        };
        assert!(message.contains(reason), "{family:?}: {message}");
        assert!(!message.contains("yet"), "{family:?}: {message}");
        let catalog = Catalog::with_sources(vec![(made, CatalogSource::Typed)], Vec::new());
        let resolved = resolve(&catalog, &[], &format!("{id}/gemini-2.5-pro"))
            .expect("the typed fallback serves the family");
        assert_eq!(resolved.entry.context_window, None, "{family:?}");
    }
}

fn copilot_billing_fixture() -> &'static [u8] {
    br#"{"data":[
        {"id":"priced-model","name":"Priced Model","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384},"supports":{"tool_calls":true,"vision":true}},"billing":{"multiplier":3,"token_prices":{"default":{"max_prompt_tokens":64000,"input_price":500,"output_price":2500,"cache_price":50}}}},
        {"id":"no-cache-leg","name":"No Cache Leg","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384}},"billing":{"token_prices":{"default":{"max_prompt_tokens":64000,"input_price":100,"output_price":200}}}},
        {"id":"bad-multiplier","name":"Bad Multiplier","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384}},"billing":{"multiplier":-2,"token_prices":{"default":{"max_prompt_tokens":64000,"input_price":100,"output_price":200,"cache_price":10}}}},
        {"id":"bad-price","name":"Bad Price","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384}},"billing":{"token_prices":{"default":{"max_prompt_tokens":64000,"input_price":-5,"output_price":200,"cache_price":10}}}}
    ]}"#
}

#[test]
fn copilot_live_rows_retain_premium_multiplier_and_flat_price() {
    let chat = provider(
        "github-copilot",
        Family::Chat,
        "https://api.githubcopilot.com",
    );
    let rows =
        decode_copilot_models(&chat, copilot_billing_fixture()).expect("copilot rows decode");
    assert_eq!(rows.len(), 4);
    let priced = &rows[0];
    assert_eq!(priced.id.as_ref(), "priced-model");
    assert_eq!(priced.premium_multiplier, Some(3.0));
    let price = priced.price.as_ref().expect("complete live card is priced");
    assert_eq!(price.input, 5.0);
    assert_eq!(price.output, 25.0);
    assert_eq!(price.cached_input, 0.5);
    assert_eq!(price.cache_write, None);
    assert_eq!(price.reasoning, 0.0);
    assert!(price.tiers.is_empty());
}

#[test]
fn copilot_live_rows_leave_unpriced_legs_unknown_and_reject_bad_rates() {
    let chat = provider(
        "github-copilot",
        Family::Chat,
        "https://api.githubcopilot.com",
    );
    let rows =
        decode_copilot_models(&chat, copilot_billing_fixture()).expect("copilot rows decode");
    assert_eq!(rows.len(), 4);
    let bare = &rows[1];
    assert_eq!(bare.premium_multiplier, None);
    assert_eq!(
        bare.price, None,
        "a non-Haiku card missing its cache leg stays unpriced, never fabricated"
    );

    let multiplier = &rows[2];
    assert_eq!(multiplier.premium_multiplier, None);
    assert!(multiplier.price.is_some());

    let unpriced = &rows[3];
    assert_eq!(unpriced.premium_multiplier, None);
    assert_eq!(unpriced.price, None);
}

fn copilot_haiku_fixture() -> &'static [u8] {
    br#"{"data":[
        {"id":"claude-haiku-5.5","name":"Claude Haiku 5.5","capabilities":{"type":"chat","limits":{"max_context_window_tokens":1000000,"max_output_tokens":64000,"max_prompt_tokens":936000},"supports":{"tool_calls":true,"vision":true}},"billing":{"multiplier":1,"token_prices":{"default":{"max_prompt_tokens":200000,"input_price":100,"output_price":300},"long_context":{"max_prompt_tokens":936000,"input_price":200,"output_price":400}}}}
    ]}"#
}

#[test]
fn haiku_cards_missing_wire_cache_legs_keep_pinned_overrides() {
    let anthropic = provider(
        "github-copilot-anthropic",
        Family::Anthropic,
        "https://api.githubcopilot.com",
    );
    let rows =
        decode_copilot_models(&anthropic, copilot_haiku_fixture()).expect("haiku rows decode");
    assert_eq!(rows.len(), 2);
    let (base, sibling) = (&rows[0], &rows[1]);
    assert_eq!(base.id.as_ref(), "claude-haiku-5.5");
    assert_eq!(sibling.id.as_ref(), "claude-haiku-5.5-1m");
    assert_eq!(base.premium_multiplier, Some(1.0));
    assert_eq!(sibling.premium_multiplier, Some(1.0));
    let base_price = base.price.as_ref().expect("haiku base is priced");
    assert_eq!(base_price.input, 1.0);
    assert_eq!(base_price.output, 3.0);
    assert_eq!(base_price.cached_input, 0.01);
    assert_eq!(base_price.cache_write, Some(0.125));
    assert!(base_price.tiers.is_empty());
    let long_price = sibling.price.as_ref().expect("haiku sibling is priced");
    assert_eq!(long_price.input, 2.0);
    assert_eq!(long_price.output, 4.0);
    assert_eq!(long_price.cached_input, 0.05);
    assert_eq!(long_price.cache_write, Some(0.625));
    assert!(long_price.tiers.is_empty());
}

#[tokio::test]
async fn billing_metadata_survives_cache_roundtrip_and_resolution() {
    use dal_core::{ModelPrice, PriceTier};

    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut billed = row("anthropic", "billing-model", Some(32_000));
    billed.premium_multiplier = Some(2.5);
    billed.price = Some(ModelPrice {
        input: 1.0,
        cached_input: 0.5,
        output: 2.0,
        reasoning: 0.25,
        cache_write: Some(0.125),
        tiers: vec![PriceTier {
            size: 100,
            input: Some(10.0),
            cached_input: None,
            cache_write: None,
            output: None,
            reasoning: None,
        }]
        .into_boxed_slice(),
    });
    let unbilled = row("anthropic", "plain-model", Some(32_000));
    write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![billed.clone(), unbilled],
    )
    .await
    .expect("write billing cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read billing cache");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].premium_multiplier, Some(2.5));
    assert_eq!(entries[0].price, billed.price);
    assert_eq!(entries[1].premium_multiplier, None);
    assert_eq!(entries[1].price, None);

    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        entries,
    );
    let resolved =
        resolve(&catalog, &[], "anthropic/billing-model").expect("billed model resolves");
    assert_eq!(resolved.entry.premium_multiplier, Some(2.5));
    assert_eq!(resolved.entry.price, billed.price);
}

#[tokio::test]
async fn stale_derived_cache_without_billing_refreshes_through_typed_fallback() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    fs::write(
        &cache_path,
        br#"{"version":1,"entries":[{"provider":"openai-codex","id":"stale-model","display":"Stale Model","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false}]}"#,
    )
    .expect("write stale derived cache");
    let stale = read_cache_async(cache_path.clone()).await;
    assert!(
        matches!(&stale, Err(CacheError::Invalid(_))),
        "derived rows without billing are stale, not known-empty: {stale:?}"
    );

    fs::write(
        &cache_path,
        br#"{"version":1,"entries":[{"provider":"openai-codex","id":"stale-model","display":"Stale Model","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false,"billing":{"premium_multiplier":-1.0,"price":null}}]}"#,
    )
    .expect("write invalid billing cache");
    let invalid = read_cache_async(cache_path.clone()).await;
    assert!(
        matches!(&invalid, Err(CacheError::Invalid(_))),
        "negative premium metadata is rejected: {invalid:?}"
    );

    fs::write(
        &cache_path,
        br#"{"version":1,"entries":[{"provider":"openai-codex","id":"stale-model","display":"Stale Model","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false}]}"#,
    )
    .expect("restore stale derived cache");
    let port = closed_loopback_port();
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let credential = oauth(Some("account"));
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    assert!(fetched.live_error.is_some());
    assert!(
        fetched
            .cache_error
            .as_ref()
            .is_some_and(|error| error.contains("billing")),
        "stale billing surfaces through the refresh path: {:?}",
        fetched.cache_error
    );
}
