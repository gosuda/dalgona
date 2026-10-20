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
    prices::{PRICE_ROWS, PRICE_TIER_ROWS, PriceRow, TierRow, tier_rows_are_sorted_unique},
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
        provider: None,
    };
    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    assert_eq!(
        resolve(&typed, &[], "gpt-6").unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::from("gpt-6"),
        },
        "bare id is unknown",
    );
    let resolved = resolve_route(&typed, &[], &route).expect("family provider serves the id");
    assert_eq!(resolved.provider.as_ref(), "openai");
    assert_eq!(resolved.route.provider(), Some("openai"));
    assert!(
        resolved.route.same_model(&route),
        "the retry keeps the requested model"
    );

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
        (Family::Chat, "my-chat", "https://example.test/v1"),
        (Family::Responses, "my-responses", "https://example.test/v1"),
        (Family::Codex, "my-codex", "https://example.test/codex"),
        (Family::Anthropic, "my-anthropic", "https://example.test"),
        (
            Family::Bedrock,
            "my-bedrock",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        (Family::Cursor, "my-cursor", "https://api.cursor.com"),
        (Family::Devin, "my-devin", "https://server.codeium.com"),
        (Family::Droid, "factory-droid", "https://api.factory.ai"),
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
        (Family::Mistral, "my-mistral", "https://api.mistral.ai"),
        (Family::Ollama, "my-ollama", "http://10.0.0.9:11434"),
        (
            Family::PiMessages,
            "my-pimessages",
            "https://example.test/pi",
        ),
    ];
    for (family, id, base) in cases {
        let typed = Catalog::with_sources(
            vec![(provider(id, family, base), CatalogSource::Typed)],
            Vec::new(),
        );
        let route = ModelRoute::Api {
            family,
            model: "some-new-id".into(),
            provider: None,
        };
        assert_eq!(
            resolve(&typed, &[], "some-new-id").unwrap_err(),
            ResolveError::UnknownModel {
                reference: String::from("some-new-id"),
            },
            "{id}: bare id stays unknown on a typed catalog",
        );
        let resolved = resolve_route(&typed, &[], &route)
            .unwrap_or_else(|error| panic!("{id}: single owner serves a typed id: {error:?}"));
        assert_eq!(resolved.provider.as_ref(), id);
        assert_eq!(resolved.route.provider(), Some(id));
        assert!(
            resolved.route.same_model(&route),
            "{id}: the retry keeps the requested model"
        );
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
        provider: None,
    };
    assert_eq!(
        resolve(&catalog, &[], "ollama-cloud/gpt-oss:120b")
            .expect("qualified cloud id resolves")
            .provider
            .as_ref(),
        "ollama-cloud",
        "the qualified id keeps its own provider",
    );
    assert_eq!(
        resolve_route(&catalog, &[], &route)
            .unwrap_err()
            .to_string(),
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
        let typed = Catalog::with_sources(
            vec![(provider(id, Family::Ollama, base), CatalogSource::Typed)],
            Vec::new(),
        );
        let route = ModelRoute::Api {
            family: Family::Ollama,
            model: "some-new-id".into(),
            provider: None,
        };
        assert_eq!(
            resolve(&typed, &[], "some-new-id").unwrap_err(),
            ResolveError::UnknownModel {
                reference: String::from("some-new-id"),
            },
            "{id}: bare id stays unknown on a typed catalog",
        );
        let resolved = resolve_route(&typed, &[], &route)
            .unwrap_or_else(|error| panic!("{id}: single owner serves a typed id: {error:?}"));
        assert_eq!(resolved.provider.as_ref(), id);
        assert_eq!(resolved.route.provider(), Some(id));
        assert!(
            resolved.route.same_model(&route),
            "{id}: the retry keeps the requested model"
        );
    }
}

#[test]
fn route_retry_serves_single_vendor_chat_and_responses_owners() {
    for (id, family, base) in [
        ("openai", Family::Chat, "https://api.openai.com/v1"),
        ("openai", Family::Responses, "https://api.openai.com/v1"),
    ] {
        let typed = Catalog::with_sources(
            vec![(provider(id, family, base), CatalogSource::Typed)],
            Vec::new(),
        );
        let route = ModelRoute::Api {
            family,
            model: "some-new-id".into(),
            provider: None,
        };
        assert_eq!(
            resolve(&typed, &[], "some-new-id").unwrap_err(),
            ResolveError::UnknownModel {
                reference: String::from("some-new-id"),
            },
        );
        let resolved = resolve_route(&typed, &[], &route).unwrap_or_else(|error| {
            panic!("{id:?}/{family:?}: vendor single serves typed id: {error:?}")
        });
        assert_eq!(resolved.provider.as_ref(), id);
        assert_eq!(resolved.route.provider(), Some(id));
        assert!(
            resolved.route.same_model(&route),
            "{id:?}/{family:?}: the retry keeps the requested model"
        );
    }
}

#[test]
fn route_retry_never_guesses_among_chat_or_responses_owners() {
    // Several owners and no `openai` row serving the family: no spelling of
    // the route names one of them, so the retry stays out and the bare error
    // stands. The `openai` cases live in
    // `route_retry_among_several_owners_serves_the_canonical_openai_row`.
    let chat_dual = Catalog::with_sources(
        vec![
            (
                provider("my-chat", Family::Chat, "https://example.test/a"),
                CatalogSource::Typed,
            ),
            (
                provider("other-chat", Family::Chat, "https://example.test/b"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let chat_route = ModelRoute::Api {
        family: Family::Chat,
        model: "some-new-id".into(),
        provider: None,
    };
    assert_eq!(
        resolve(&chat_dual, &[], "my-chat/some-new-id")
            .expect("qualified custom Chat id resolves")
            .provider
            .as_ref(),
        "my-chat",
        "qualified custom Chat id keeps its identity",
    );
    assert_eq!(
        resolve_route(&chat_dual, &[], &chat_route)
            .unwrap_err()
            .to_string(),
        "unknown model some-new-id",
        "two Chat owners mean no retry guess",
    );
    let responses_dual = Catalog::with_sources(
        vec![
            (
                provider("my-responses", Family::Responses, "https://a.test/v1"),
                CatalogSource::Typed,
            ),
            (
                provider("other-responses", Family::Responses, "https://b.test/v1"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let responses_route = ModelRoute::Api {
        family: Family::Responses,
        model: "some-new-id".into(),
        provider: None,
    };
    assert_eq!(
        resolve(&responses_dual, &[], "my-responses/some-new-id")
            .expect("qualified custom Responses id resolves")
            .provider
            .as_ref(),
        "my-responses",
        "qualified custom Responses id keeps its identity",
    );
    assert_eq!(
        resolve_route(&responses_dual, &[], &responses_route)
            .unwrap_err()
            .to_string(),
        "unknown model some-new-id",
        "two custom Responses owners mean no retry guess",
    );
}

#[test]
fn route_retry_ignores_an_openai_row_serving_the_other_family() {
    // The canonical owner must serve the route's own family: an `openai` row
    // rebound to the other one is not a canonical owner, so several custom
    // owners still mean no retry.
    for (route_family, openai_family, first, second) in [
        (
            Family::Responses,
            Family::Chat,
            "my-responses",
            "other-responses",
        ),
        (Family::Chat, Family::Responses, "my-chat", "other-chat"),
    ] {
        let rebound_catalog = Catalog::with_sources(
            vec![
                (
                    provider("openai", openai_family, "https://api.openai.com/v1"),
                    CatalogSource::Typed,
                ),
                (
                    provider(first, route_family, "https://a.test/v1"),
                    CatalogSource::Typed,
                ),
                (
                    provider(second, route_family, "https://b.test/v1"),
                    CatalogSource::Typed,
                ),
            ],
            Vec::new(),
        );
        let route = ModelRoute::Api {
            family: route_family,
            model: "some-new-id".into(),
            provider: None,
        };
        assert_eq!(
            resolve_route(&rebound_catalog, &[], &route)
                .unwrap_err()
                .to_string(),
            "unknown model some-new-id",
            "{route_family:?}: an openai row serving {openai_family:?} is no canonical owner",
        );
    }
}

#[test]
fn route_retry_among_several_owners_serves_the_canonical_openai_row() {
    // `openai` and `openrouter` both serve Responses, so the census finds no
    // single owner. The `openai-responses` and `openai-chat` spellings name
    // `openai`, so a route without a provider keeps running there instead of
    // dying as an unknown model.
    for (family, other, other_url) in [
        (
            Family::Responses,
            "openrouter",
            "https://openrouter.ai/api/v1",
        ),
        (Family::Chat, "my-chat", "https://example.test/v1"),
    ] {
        let typed = Catalog::with_sources(
            vec![
                (
                    provider("openai", family, "https://api.openai.com/v1"),
                    CatalogSource::Typed,
                ),
                (provider(other, family, other_url), CatalogSource::Typed),
            ],
            Vec::new(),
        );
        let route = ModelRoute::Api {
            family,
            model: "some-new-id".into(),
            provider: None,
        };
        assert_eq!(
            resolve(&typed, &[], "some-new-id").unwrap_err(),
            ResolveError::UnknownModel {
                reference: String::from("some-new-id"),
            },
            "{family:?}: the bare id stays unknown on a typed catalog",
        );
        let resolved = resolve_route(&typed, &[], &route).unwrap_or_else(|error| {
            panic!("{family:?}: the canonical openai row serves the route: {error:?}")
        });
        assert_eq!(resolved.provider.as_ref(), "openai");
        assert_eq!(resolved.route.provider(), Some("openai"));
        assert!(
            resolved.route.same_model(&route),
            "{family:?}: the retry keeps the requested model",
        );
        let qualified = resolve(&typed, &[], &format!("{other}/some-new-id"))
            .unwrap_or_else(|error| panic!("{other}: qualified id: {error:?}"));
        assert_eq!(qualified.provider.as_ref(), other);
    }

    // The retry resolves through the canonical row like any qualified id, so
    // a catalog that lists models still rejects an id that row does not list.
    let listed = Catalog::with_sources(
        vec![
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("openrouter", Family::Responses, "https://or.test/v1"),
                CatalogSource::Cache,
            ),
        ],
        vec![row("openai", "known-model", Some(10_000))],
    );
    let unlisted = ModelRoute::Api {
        family: Family::Responses,
        model: "some-new-id".into(),
        provider: None,
    };
    assert_eq!(
        resolve_route(&listed, &[], &unlisted)
            .unwrap_err()
            .to_string(),
        "unknown model some-new-id",
        "a listed catalog still rejects an id the canonical row does not list",
    );
}

#[test]
fn route_retry_resolves_a_listed_shared_id_through_the_canonical_openai_row() {
    // Several providers of one family list the same id, so the bare id is
    // ambiguous. A route without a provider keeps resolving through `openai`;
    // a route that names its provider selects exactly that row, never `openai`.
    for (family, other, other_url) in [
        (
            Family::Responses,
            "openrouter",
            "https://openrouter.ai/api/v1",
        ),
        (Family::Chat, "my-chat", "https://example.test/v1"),
    ] {
        let catalog = Catalog::with_sources(
            vec![
                (
                    provider("openai", family, "https://api.openai.com/v1"),
                    CatalogSource::Cache,
                ),
                (provider(other, family, other_url), CatalogSource::Cache),
            ],
            vec![
                row("openai", "shared-id", Some(11)),
                row(other, "shared-id", Some(22)),
            ],
        );
        assert!(
            matches!(
                resolve(&catalog, &[], "shared-id"),
                Err(ResolveError::AmbiguousModel { .. })
            ),
            "{family:?}: the bare id is ambiguous",
        );
        let route = ModelRoute::Api {
            family,
            model: "shared-id".into(),
            provider: None,
        };
        let resolved = resolve_route(&catalog, &[], &route).unwrap_or_else(|error| {
            panic!("{family:?}: the canonical openai row serves the listed id: {error:?}")
        });
        assert_eq!(resolved.provider.as_ref(), "openai");
        assert_eq!(resolved.route.provider(), Some("openai"));
        assert_eq!(resolved.entry.context_window, Some(11));
        assert!(
            resolved.route.same_model(&route),
            "{family:?}: the retry keeps the requested model",
        );
        for (named, window) in [("openai", 11), (other, 22)] {
            let exact = resolve_route(&catalog, &[], &route.with_provider(named))
                .unwrap_or_else(|error| panic!("{named}: exact route: {error:?}"));
            assert_eq!(exact.provider.as_ref(), named);
            assert_eq!(exact.route, route.with_provider(named));
            assert_eq!(exact.entry.context_window, Some(window));
        }
    }
}

#[test]
fn route_retry_keeps_ambiguity_between_custom_owners_of_a_listed_id() {
    // No `openai` row serves the route's family, so a listed id that two
    // custom owners share stays ambiguous for a route without a provider.
    for (route_family, openai_family) in [
        (Family::Responses, Family::Chat),
        (Family::Chat, Family::Responses),
    ] {
        let custom_catalog = Catalog::with_sources(
            vec![
                (
                    provider("openai", openai_family, "https://api.openai.com/v1"),
                    CatalogSource::Cache,
                ),
                (
                    provider("custom-a", route_family, "https://a.test/v1"),
                    CatalogSource::Cache,
                ),
                (
                    provider("custom-b", route_family, "https://b.test/v1"),
                    CatalogSource::Cache,
                ),
            ],
            vec![
                row("custom-a", "shared-id", Some(1)),
                row("custom-b", "shared-id", Some(2)),
            ],
        );
        let route = ModelRoute::Api {
            family: route_family,
            model: "shared-id".into(),
            provider: None,
        };
        let error = resolve_route(&custom_catalog, &[], &route)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("model id shared-id matches several providers: "),
            "{route_family:?}: custom owners stay ambiguous, got: {error}",
        );
        assert!(error.contains("custom-a/shared-id"), "{error}");
        assert!(error.contains("custom-b/shared-id"), "{error}");
    }
}

#[test]
fn route_retry_without_owner_keeps_bare_error() {
    let empty = Catalog::with_sources(Vec::new(), Vec::new());
    let route = ModelRoute::Api {
        family: Family::Ollama,
        model: "some-new-id".into(),
        provider: None,
    };
    assert_eq!(
        resolve_route(&empty, &[], &route).unwrap_err().to_string(),
        "unknown model some-new-id",
        "zero owners return the bare error without a degenerate probe",
    );
    let other_family = Catalog::with_sources(
        vec![(
            provider("my-chat", Family::Chat, "https://example.test/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let ollama_route = ModelRoute::Api {
        family: Family::Ollama,
        model: "some-new-id".into(),
        provider: None,
    };
    assert_eq!(
        resolve_route(&other_family, &[], &ollama_route)
            .unwrap_err()
            .to_string(),
        "unknown model some-new-id",
        "a family with no owner keeps the bare error",
    );
}

#[test]
fn route_retry_keeps_ambiguous_bare_error() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("ollama", Family::Ollama, "http://127.0.0.1:11434"),
                CatalogSource::Cache,
            ),
            (
                provider("ollama-cloud", Family::Ollama, "https://ollama.com"),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("ollama", "shared-id", Some(10_000)),
            row("ollama-cloud", "shared-id", Some(10_000)),
        ],
    );
    let route = ModelRoute::Api {
        family: Family::Ollama,
        model: "shared-id".into(),
        provider: None,
    };
    let error = resolve_route(&catalog, &[], &route)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("model id shared-id matches several providers: "),
        "dual listed owners keep the ambiguous error, got: {error}",
    );
    assert!(error.contains("ollama/shared-id"));
    assert!(error.contains("ollama-cloud/shared-id"));
}

#[test]
fn route_retry_keeps_explicit_provider_identity() {
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
    let cloud = resolve(&catalog, &[], "ollama-cloud/gpt-oss:120b")
        .unwrap_or_else(|error| panic!("qualified cloud keeps identity: {error:?}"));
    assert_eq!(cloud.provider.as_ref(), "ollama-cloud");
    let local = resolve(&catalog, &[], "ollama/gpt-oss:120b")
        .unwrap_or_else(|error| panic!("qualified local keeps identity: {error:?}"));
    assert_eq!(local.provider.as_ref(), "ollama");
    assert_eq!(
        resolve(&catalog, &[], "openai/gpt-oss:120b")
            .unwrap_err()
            .to_string(),
        "unknown model openai/gpt-oss:120b",
        "an unconfigured provider id stays unknown",
    );
    let openai_chat = Catalog::with_sources(
        vec![(
            provider("openai", Family::Chat, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let explicit = resolve(&openai_chat, &[], "openai-chat/some-new-id")
        .unwrap_or_else(|error| panic!("explicit openai-chat keeps identity: {error:?}"));
    assert_eq!(explicit.provider.as_ref(), "openai");
}

#[test]
fn provider_routes_select_their_own_row_for_a_shared_native_id() {
    // Two custom Responses owners list one native id with different rows.
    // Custom ids keep the provider-less route on the legacy bare path: no
    // single owner and no canonical row exist, so the ambiguity stands.
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("acme-a", Family::Responses, "https://a.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("acme-b", Family::Responses, "https://b.test/v1"),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("acme-a", "probe-shared-7", Some(11)),
            row("acme-b", "probe-shared-7", Some(22)),
        ],
    );
    let a_route = ModelRoute::Api {
        family: Family::Responses,
        model: "probe-shared-7".into(),
        provider: Some("acme-a".into()),
    };
    let a = resolve_route(&catalog, &[], &a_route).expect("acme-a route resolves");
    assert_eq!(a.provider.as_ref(), "acme-a");
    assert_eq!(a.route, a_route);
    assert_eq!(a.entry.provider.as_ref(), "acme-a");
    assert_eq!(a.entry.context_window, Some(11));

    let b_route = ModelRoute::Api {
        family: Family::Responses,
        model: "probe-shared-7".into(),
        provider: Some("acme-b".into()),
    };
    let b = resolve_route(&catalog, &[], &b_route).expect("acme-b route resolves");
    assert_eq!(b.provider.as_ref(), "acme-b");
    assert_eq!(b.route, b_route);
    assert_eq!(b.entry.provider.as_ref(), "acme-b");
    assert_eq!(b.entry.context_window, Some(22));

    assert!(
        !a.route.same_model(&b.route),
        "one native id under two providers names two models"
    );

    let bare = ModelRoute::Api {
        family: Family::Responses,
        model: "probe-shared-7".into(),
        provider: None,
    };
    assert!(a.route.same_model(&bare));
    assert!(b.route.same_model(&bare));
    assert_eq!(
        resolve(&catalog, &[], "probe-shared-7")
            .unwrap_err()
            .to_string(),
        "model id probe-shared-7 matches several providers: \
         acme-a/probe-shared-7, acme-b/probe-shared-7",
    );
    assert!(
        matches!(
            resolve_route(&catalog, &[], &bare),
            Err(ResolveError::AmbiguousModel { .. })
        ),
        "the provider-less route keeps the legacy ambiguity",
    );
}

#[test]
fn provider_route_native_ids_are_never_reinterpreted_as_references() {
    // A native id that spells another provider's `provider/model` text, or
    // that starts with its own provider id, still resolves in its own
    // provider: the provider-scoped path never splits the native id again.
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("first", Family::Responses, "https://first.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("second", Family::Responses, "https://second.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider(
                    "openrouter",
                    Family::Responses,
                    "https://openrouter.ai/api/v1",
                ),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("first", "shared-x", Some(1)),
            row("second", "first/shared-x", Some(2)),
            row("openrouter", "openrouter/free", Some(3)),
        ],
    );
    let nested = ModelRoute::Api {
        family: Family::Responses,
        model: "first/shared-x".into(),
        provider: Some("second".into()),
    };
    let resolved = resolve_route(&catalog, &[], &nested).expect("second owns its nested id");
    assert_eq!(resolved.provider.as_ref(), "second");
    assert_eq!(resolved.route, nested);
    assert_eq!(resolved.entry.provider.as_ref(), "second");
    assert_eq!(resolved.entry.id.as_ref(), "first/shared-x");
    assert_eq!(
        resolve(&catalog, &[], "second/first/shared-x")
            .expect("the qualified nested id agrees")
            .route,
        nested
    );

    let prefixed = ModelRoute::Api {
        family: Family::Responses,
        model: "openrouter/free".into(),
        provider: Some("openrouter".into()),
    };
    let resolved = resolve_route(&catalog, &[], &prefixed).expect("self-prefixed id resolves");
    assert_eq!(resolved.provider.as_ref(), "openrouter");
    assert_eq!(resolved.route, prefixed);
    assert_eq!(resolved.entry.id.as_ref(), "openrouter/free");
}

#[test]
fn provider_route_with_an_unconfigured_provider_never_falls_back() {
    // The named provider is not configured even though another provider
    // lists the id: the route fails typed instead of binding elsewhere.
    let catalog = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "probe-unique-9", Some(7))],
    );
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "probe-unique-9".into(),
        provider: Some("ghost".into()),
    };
    let error = resolve_route(&catalog, &[], &route).unwrap_err();
    assert_eq!(
        error,
        ResolveError::UnknownProvider {
            provider: String::from("ghost"),
            model: String::from("probe-unique-9"),
        }
    );
    assert_eq!(
        error.to_string(),
        "provider ghost is not configured for model probe-unique-9",
    );
}

#[test]
fn provider_route_with_a_foreign_family_is_a_mismatch() {
    // Each provider lists the id, but the route names a provider serving
    // another family: the row is refused rather than reinterpreted.
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("openai", "probe-d-1", Some(1)),
            row("anthropic", "probe-d-1", Some(2)),
        ],
    );
    for (family, provider) in [
        (Family::Responses, "anthropic"),
        (Family::Anthropic, "openai"),
    ] {
        let route = ModelRoute::Api {
            family,
            model: "probe-d-1".into(),
            provider: Some(provider.into()),
        };
        let error = resolve_route(&catalog, &[], &route).unwrap_err();
        assert_eq!(
            error,
            ResolveError::ProviderFamilyMismatch {
                provider: String::from(provider),
                model: String::from("probe-d-1"),
            },
            "{provider}: wrong-family row is refused",
        );
        assert_eq!(
            error.to_string(),
            format!("provider {provider} does not serve the API family of model probe-d-1"),
        );
    }
}

#[test]
fn provider_route_is_never_alias_expanded() {
    // `probe-alias` expands to a listed id, but the provider-scoped path
    // skips alias expansion: the unlisted native id fails in its own
    // provider instead of resolving the alias target.
    let catalog = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "probe-target-5", Some(1))],
    );
    let aliases = [(
        Box::<str>::from("probe-alias"),
        Box::<str>::from("openai/probe-target-5"),
    )];
    let expanded = resolve(&catalog, &aliases, "probe-alias")
        .expect("the bare reference still expands the alias");
    assert_eq!(expanded.provider.as_ref(), "openai");
    assert_eq!(expanded.route.id(), "probe-target-5");
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "probe-alias".into(),
        provider: Some("openai".into()),
    };
    assert_eq!(
        resolve_route(&catalog, &aliases, &route)
            .unwrap_err()
            .to_string(),
        "unknown model openai/probe-alias",
        "the provider route names its own unlisted id",
    );
}

#[test]
fn qualified_and_bare_resolves_carry_the_provider() {
    // Every `resolve` path through `resolve_qualified` stamps the producing
    // provider on the route: qualified ids, unique bare ids, and typed ids.
    let catalog = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "probe-solo-3", Some(7))],
    );
    let qualified = resolve(&catalog, &[], "openai/probe-solo-3").expect("qualified resolves");
    assert_eq!(qualified.provider.as_ref(), "openai");
    assert_eq!(qualified.route.provider(), Some("openai"));
    let bare = resolve(&catalog, &[], "probe-solo-3").expect("unique bare id resolves");
    assert_eq!(bare.provider.as_ref(), "openai");
    assert_eq!(bare.route.provider(), Some("openai"));
    assert_eq!(bare.route, qualified.route);

    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let fresh = resolve(&typed, &[], "openai/probe-fresh-4").expect("typed id resolves");
    assert_eq!(fresh.route.provider(), Some("openai"));
    assert_eq!(fresh.entry.provider.as_ref(), "openai");
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
        provider: None,
    };
    let resolved = resolve_route(&catalog, &[], &route).expect("listed id resolves");
    assert_eq!(resolved.provider.as_ref(), "acme");
    assert_eq!(resolved.route.provider(), Some("acme"));
    assert!(
        resolved.route.same_model(&route),
        "the listed row keeps the requested model"
    );
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
            provider: None,
        };
        let resolved = resolve_route(&catalog, &[], &route).expect("family picks the owner");
        assert_eq!(resolved.provider.as_ref(), owner);
        assert_eq!(resolved.route.provider(), Some(owner));
        assert!(
            resolved.route.same_model(&route),
            "{owner}: the family row keeps the requested model"
        );
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
        provider: None,
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
        provider: None,
    };
    assert_eq!(
        resolve_route(&catalog, &alias, &aliased).unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::from("pick"),
        },
        "the error names the route's model, not the alias target another family owns",
    );
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
fn a_provider_that_lists_one_id_twice_is_not_ambiguous_with_itself() {
    let provider = provider("my-openai", Family::Chat, "https://example.test/v1");
    let rows = decode_openai_models(&provider, br#"{"data":[{"id":"dup"},{"id":"dup"}]}"#)
        .expect("duplicate ids decode");
    let catalog = Catalog::with_sources(vec![(provider, CatalogSource::Live)], rows);
    let resolved = resolve(&catalog, &[], "dup").expect("one provider owns the id");
    assert_eq!(resolved.provider.as_ref(), "my-openai");
}

#[test]
fn a_built_in_row_of_an_unconfigured_provider_cannot_be_resolved_any_spelling() {
    // The catalog adds every built-in row, but only configured providers can
    // serve one: an unconfigured `openai` leaves its built-ins unknown.
    let catalog = Catalog::with_sources(Vec::new(), Vec::new());
    assert!(
        catalog
            .entries()
            .iter()
            .any(|entry| entry.provider.as_ref() == "openai" && entry.id.as_ref() == "gpt-6-sol"),
        "the built-in row is present",
    );
    for reference in [
        "gpt-6-sol",
        "openai/gpt-6-sol",
        "openai-responses/gpt-6-sol",
    ] {
        assert_eq!(
            resolve(&catalog, &[], reference).unwrap_err(),
            ResolveError::UnknownModel {
                reference: reference.to_owned(),
            },
            "{reference}",
        );
    }
    let route = ModelRoute::Api {
        family: Family::Responses,
        model: "gpt-6-sol".into(),
        provider: None,
    };
    assert_eq!(
        resolve_route(&catalog, &[], &route).unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::from("gpt-6-sol"),
        },
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
fn date_suffix_sends_the_requested_id_with_base_capabilities() {
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
    assert_eq!(resolved.entry.id.as_ref(), "claude-sonnet-5-20260101");
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
fn a_date_suffix_strips_to_its_base_row_only_when_it_is_exactly_eight_ascii_digits() {
    // N-1, N, N+1 digits, a letter and multi-byte digits at N bytes, and a
    // dashed date: only the closed `-YYYYMMDD` shape names its base row.
    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        vec![row("anthropic", "claude-sonnet-5", Some(200_000))],
    );
    for (reference, resolves) in [
        ("claude-sonnet-5-2026010", false),
        ("claude-sonnet-5-20260101", true),
        ("claude-sonnet-5-202601011", false),
        ("claude-sonnet-5-2026010a", false),
        ("claude-sonnet-5-2026\u{662}\u{660}", false),
        ("claude-sonnet-5-2026-01-01", false),
        ("claude-sonnet-5-latest", true),
        ("claude-sonnet-5-latest-2", false),
    ] {
        let outcome = resolve(&catalog, &[], reference);
        if resolves {
            let resolved = outcome.unwrap_or_else(|error| panic!("{reference}: {error:?}"));
            assert_eq!(resolved.route.id(), reference);
            assert_eq!(resolved.entry.context_window, Some(200_000), "{reference}");
        } else {
            assert_eq!(
                outcome.unwrap_err(),
                ResolveError::UnknownModel {
                    reference: reference.to_owned(),
                },
                "{reference}",
            );
        }
    }
}

#[test]
fn an_exact_listed_id_outranks_the_base_row_its_date_suffix_strips_to() {
    // `beta` lists the dated id and the base; `alpha` lists only the base. If
    // the stripped base were tried first, the bare id would collide across
    // providers and the qualified id would read the wrong row.
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("alpha", Family::Chat, "https://alpha.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("beta", Family::Chat, "https://beta.test/v1"),
                CatalogSource::Cache,
            ),
        ],
        vec![
            row("alpha", "m", Some(1)),
            row("beta", "m", Some(3)),
            row("beta", "m-20260101", Some(2)),
        ],
    );
    let bare = resolve(&catalog, &[], "m-20260101").expect("only beta lists the dated id");
    assert_eq!(bare.provider.as_ref(), "beta");
    assert_eq!(bare.entry.context_window, Some(2));
    let qualified =
        resolve(&catalog, &[], "beta/m-20260101").expect("the qualified dated id resolves");
    assert_eq!(qualified.entry.context_window, Some(2));
    assert!(
        matches!(
            resolve(&catalog, &[], "m"),
            Err(ResolveError::AmbiguousModel { .. })
        ),
        "the base id is still shared by both providers",
    );
}

#[test]
fn an_empty_native_id_is_unknown_even_where_a_typed_provider_accepts_new_ids() {
    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    assert_eq!(
        resolve(&typed, &[], "openai/").unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::from("openai/"),
        },
    );
    let scoped = ModelRoute::Api {
        family: Family::Responses,
        model: "".into(),
        provider: Some("openai".into()),
    };
    assert_eq!(
        resolve_route(&typed, &[], &scoped).unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::from("openai/"),
        },
    );
    let unscoped = ModelRoute::Api {
        family: Family::Responses,
        model: "".into(),
        provider: None,
    };
    assert_eq!(
        resolve_route(&typed, &[], &unscoped).unwrap_err(),
        ResolveError::UnknownModel {
            reference: String::new(),
        },
    );
}

#[test]
fn harness_references_resolve_by_id_and_no_configured_provider_can_shadow_them() {
    // `dalgon` is a legal provider id; a typed provider with it must not turn
    // an unknown harness mode into a routable model or an Api route.
    let catalog = Catalog::with_sources(
        vec![(
            provider("dalgon", Family::Chat, "https://dalgon.test/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let aliases = [(
        Box::<str>::from("fast"),
        Box::<str>::from("dalgon/eval-only"),
    )];
    for (reference, mode) in [
        ("dalgon/normal", "dalgon/normal"),
        ("dalgon/eval-first", "dalgon/eval-first"),
        ("dalgon/eval-only", "dalgon/eval-only"),
        ("fast", "dalgon/eval-only"),
    ] {
        let resolved = resolve(&catalog, &aliases, reference)
            .unwrap_or_else(|error| panic!("{reference}: {error:?}"));
        assert_eq!(resolved.provider.as_ref(), "dalgon", "{reference}");
        assert!(
            matches!(&resolved.route, ModelRoute::Harness { id } if id.as_ref() == mode),
            "{reference}: {:?}",
            resolved.route,
        );
        assert_eq!(
            resolved.entry.tool_support,
            ToolSupport::None,
            "{reference}"
        );
        assert_eq!(resolved.entry.context_window, None, "{reference}");
    }
    for hostile in [
        "dalgon/",
        "dalgon/bogus",
        "dalgon/normal/",
        "dalgon/Normal",
        "dalgon/normal ",
    ] {
        assert_eq!(
            resolve(&catalog, &aliases, hostile).unwrap_err(),
            ResolveError::UnknownModel {
                reference: hostile.to_owned(),
            },
            "{hostile:?}",
        );
    }
}

#[test]
fn tool_support_allows_exactly_its_documented_family_and_level_cells() {
    const FAMILIES: [Family; 16] = [
        Family::Chat,
        Family::Responses,
        Family::Codex,
        Family::Anthropic,
        Family::Bedrock,
        Family::Cursor,
        Family::Devin,
        Family::Droid,
        Family::GitLab,
        Family::GoogleGenerativeAi,
        Family::GoogleVertex,
        Family::GoogleGeminiCli,
        Family::GoogleAntigravity,
        Family::Mistral,
        Family::Ollama,
        Family::PiMessages,
    ];
    const LEVELS: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    for family in FAMILIES {
        for level in LEVELS {
            let cell = format!("{family:?}/{level:?}");
            assert!(!ToolSupport::None.allows(family, level), "None {cell}");
            assert!(ToolSupport::Any.allows(family, level), "Any {cell}");
            let responses_style = matches!(family, Family::Responses | Family::Codex);
            assert_eq!(
                ToolSupport::ResponsesOnly.allows(family, level),
                responses_style,
                "ResponsesOnly {cell}",
            );
            let chat_without_reasoning = family == Family::Chat && level == ThinkingLevel::Off;
            assert_eq!(
                ToolSupport::ChatWhenNoReasoning.allows(family, level),
                responses_style || chat_without_reasoning,
                "ChatWhenNoReasoning {cell}",
            );
        }
    }
}

#[test]
fn built_in_rows_are_added_unless_the_exact_provider_and_id_pair_is_already_present() {
    // Same pair: the fetched row stands alone. Same provider with another id,
    // or another provider with a built-in's id: the built-in stays.
    let catalog = Catalog::with_sources(
        Vec::new(),
        vec![
            row("openai", "gpt-6-sol", Some(1)),
            row("openai", "not-a-built-in", Some(2)),
            row("openrouter", "gpt-5.6", Some(3)),
        ],
    );
    let count = |provider: &str, id: &str| {
        catalog
            .entries()
            .iter()
            .filter(|entry| entry.provider.as_ref() == provider && entry.id.as_ref() == id)
            .count()
    };
    assert_eq!(
        count("openai", "gpt-6-sol"),
        1,
        "the fetched row is not doubled"
    );
    assert_eq!(
        count("openai", "gpt-5.6"),
        1,
        "a sibling id keeps its built-in"
    );
    assert_eq!(count("openrouter", "gpt-5.6"), 1);
    assert_eq!(count("openai", "not-a-built-in"), 1);
    assert_eq!(count("openai-codex", "gpt-reserve"), 1);
    assert_eq!(
        catalog.entries().len(),
        built_in_entries().len() + 2,
        "three fetched rows overlap exactly one built-in",
    );
    let fetched = catalog
        .entries()
        .iter()
        .find(|entry| entry.provider.as_ref() == "openai" && entry.id.as_ref() == "gpt-6-sol")
        .expect("the gpt-6-sol row exists");
    assert_eq!(fetched.context_window, Some(1), "the fetched row wins");
}

#[test]
fn only_openai_style_families_borrow_built_in_capabilities_for_a_typed_id() {
    for (family, id, base) in [
        (Family::Anthropic, "my-anthropic", "https://example.test"),
        (Family::Mistral, "my-mistral", "https://api.mistral.ai/v1"),
        (Family::Ollama, "my-ollama", "http://10.0.0.9:11434"),
        (
            Family::GoogleGenerativeAi,
            "my-google",
            "https://generativelanguage.googleapis.com/v1beta",
        ),
        (Family::PiMessages, "my-pi", "https://example.test/pi"),
    ] {
        let catalog = Catalog::with_sources(
            vec![(provider(id, family, base), CatalogSource::Typed)],
            Vec::new(),
        );
        let resolved = resolve(&catalog, &[], &format!("{id}/gpt-5.6-luna"))
            .unwrap_or_else(|error| panic!("{family:?}: {error:?}"));
        assert_eq!(resolved.entry.context_window, None, "{family:?}");
        assert_eq!(resolved.entry.max_output, None, "{family:?}");
        assert!(!resolved.entry.image_input, "{family:?}");
    }
    let chat = Catalog::with_sources(
        vec![(
            provider("my-chat", Family::Chat, "https://example.test/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let borrowed = resolve(&chat, &[], "my-chat/gpt-5.6-luna").expect("a typed Chat id resolves");
    assert_eq!(borrowed.entry.context_window, Some(922_000));
    assert!(borrowed.entry.image_input);
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

fn levels_of(entry: &CatalogEntry) -> (Vec<ThinkingLevel>, bool) {
    match &entry.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => (accepted.clone(), *none_supported),
        other => panic!("{} carries {other:?}", entry.id),
    }
}

#[test]
fn mistral_cards_dedupe_after_trimming_and_sanitising_their_ids() {
    let provider = provider("mistral", Family::Mistral, "https://api.mistral.ai/v1");
    let rows = decode_mistral_models(
        &provider,
        br#"{"data":[
            {"id":" padded ","max_context_length":1},
            {"id":"padded","max_context_length":2},
            {"id":"a\u0007b","max_context_length":3},
            {"id":"a\u0008b","max_context_length":4},
            {"id":"live","archived":false,"max_context_length":5},
            {"id":"gone","archived":true,"max_context_length":6},
            {"id":"   ","max_context_length":7}
        ]}"#,
    )
    .expect("mistral rows decode");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_ref(), row.context_window))
            .collect::<Vec<_>>(),
        [
            ("padded", Some(1)),
            ("a\u{FFFD}b", Some(3)),
            ("live", Some(5))
        ],
        "the first card of each trimmed, sanitised id wins"
    );
}

#[test]
fn mistral_context_length_is_positive_and_fits_u32_or_the_list_is_a_protocol_error() {
    let provider = provider("mistral", Family::Mistral, "https://api.mistral.ai/v1");
    for (length, expected) in [
        ("0", None),
        ("1", Some(1_u32)),
        ("4294967295", Some(u32::MAX)),
    ] {
        let body = format!(r#"{{"data":[{{"id":"m","max_context_length":{length}}}]}}"#);
        let rows = decode_mistral_models(&provider, body.as_bytes())
            .unwrap_or_else(|error| panic!("{length}: {error:?}"));
        assert_eq!(rows[0].context_window, expected, "{length}");
    }
    for length in ["4294967296", "4294967297"] {
        let body = format!(r#"{{"data":[{{"id":"m","max_context_length":{length}}}]}}"#);
        match decode_mistral_models(&provider, body.as_bytes()) {
            Err(ProviderError::Protocol {
                family: Family::Mistral,
                detail,
            }) => assert!(
                detail.contains("outside the supported range"),
                "{length}: {detail}"
            ),
            other => panic!("{length}: a window above u32::MAX must not wrap: {other:?}"),
        }
    }
}

#[test]
fn ollama_tags_trim_ids_and_dedupe_after_trimming_and_sanitising() {
    let provider = provider("local-ollama", Family::Ollama, "http://localhost:11434");
    let rows = decode_ollama_models(
        &provider,
        br#"{"models":[
            {"name":" llama3.1 "},
            {"model":"llama3.1"},
            {"name":"a\u0007b"},
            {"name":"a\u0008b"},
            {"model":"mistral-small","name":"  Mistral Small  "}
        ]}"#,
    )
    .expect("ollama rows decode");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_ref(), row.display.as_ref()))
            .collect::<Vec<_>>(),
        [
            ("llama3.1", "llama3.1"),
            ("a\u{FFFD}b", "a\u{FFFD}b"),
            ("mistral-small", "Mistral Small"),
        ],
    );
}

#[test]
fn openai_model_lists_that_are_not_a_data_array_of_id_rows_are_protocol_errors() {
    for family in [Family::Chat, Family::Responses] {
        let provider = provider("my-openai", family, "https://example.test/v1");
        for body in [
            &b""[..],
            b"{}",
            b"[]",
            br#"{"data":null}"#,
            br#"{"data":{}}"#,
            br#"{"data":[{}]}"#,
            br#"{"data":[{"id":7}]}"#,
            br#"{"data":[{"id":"a"}"#,
        ] {
            assert!(
                matches!(
                    decode_openai_models(&provider, body),
                    Err(ProviderError::Protocol { family: found, .. }) if found == family
                ),
                "{family:?}: {}",
                String::from_utf8_lossy(body)
            );
        }
    }
}

#[test]
fn codex_reasoning_levels_split_none_from_named_levels_and_ignore_unknown_spellings() {
    let provider = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    let bytes = br#"{"models":[
        {"slug":"levels","visibility":"list","supported_reasoning_levels":["none","minimal",{"effort":"xhigh"},{"effort":"max"},"bogus",{"effort":null},{}]},
        {"slug":"object-none","visibility":"list","supported_reasoning_levels":[{"effort":"none"},"low"]},
        {"slug":"plain","visibility":"list","supported_reasoning_levels":["low","high"]},
        {"slug":"bare","visibility":"list"}
    ]}"#;
    let rows = decode_codex_models(&provider, bytes).expect("Codex rows decode");
    assert_eq!(
        levels_of(&rows[0]),
        (
            vec![
                ThinkingLevel::Minimal,
                ThinkingLevel::Xhigh,
                ThinkingLevel::Max
            ],
            true
        ),
    );
    assert_eq!(levels_of(&rows[1]), (vec![ThinkingLevel::Low], true));
    assert_eq!(
        levels_of(&rows[2]),
        (vec![ThinkingLevel::Low, ThinkingLevel::High], false)
    );
    assert_eq!(levels_of(&rows[3]), (Vec::new(), false));
}

#[test]
fn codex_rows_are_listed_only_when_the_server_says_list() {
    let provider = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    let bytes = br#"{"models":[
        {"slug":"listed","visibility":"list"},
        {"slug":"absent"},
        {"slug":"hidden","visibility":"hide"},
        {"slug":"other","visibility":"none"},
        {"slug":"empty","visibility":""}
    ]}"#;
    let rows = decode_codex_models(&provider, bytes).expect("Codex rows decode");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_ref(), row.listing))
            .collect::<Vec<_>>(),
        [
            ("listed", Listing::Listed),
            ("absent", Listing::Hidden),
            ("hidden", Listing::Hidden),
            ("other", Listing::Hidden),
            ("empty", Listing::Hidden),
        ],
    );
}

#[test]
fn codex_effective_window_floors_and_never_wraps_past_u32() {
    let provider = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    for (context, percent, expected) in [
        (1000_u64, 50_u32, Some(500_u32)),
        (3, 50, Some(1)),
        (4_294_967_295, 100, Some(u32::MAX)),
        (4_294_967_295, 101, None),
    ] {
        let body = format!(
            r#"{{"models":[{{"slug":"m","visibility":"list","context_window":{context},"effective_context_window_percent":{percent}}}]}}"#
        );
        match (decode_codex_models(&provider, body.as_bytes()), expected) {
            (Ok(rows), Some(window)) => {
                assert_eq!(
                    rows[0].context_window,
                    Some(window),
                    "{context} at {percent}%"
                );
            }
            (
                Err(ProviderError::Protocol {
                    family: Family::Codex,
                    detail,
                }),
                None,
            ) => assert!(
                detail.contains("effective context window"),
                "{context} at {percent}%: {detail}"
            ),
            (other, _) => panic!("{context} at {percent}%: {other:?}"),
        }
    }
}

fn radius_row(id: &str, window: &str, max_tokens: &str, cost: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"{id}","reasoning":false,"input":["text"],"contextWindow":{window},"maxTokens":{max_tokens},"cost":{cost}}}"#
    )
}

fn radius_body(rows: &[String]) -> Vec<u8> {
    format!(
        r#"{{"baseUrl":"https://radius.pi.dev/v1","models":[{}]}}"#,
        rows.join(",")
    )
    .into_bytes()
}

#[test]
fn radius_reasoning_rows_default_to_minimal_through_high_and_need_a_mapping_for_xhigh_and_max() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let row_with = |id: &str, map: &str| {
        format!(
            r#"{{"id":"{id}","name":"{id}","reasoning":true,{map}"input":["text"],"contextWindow":4096,"maxTokens":512,"cost":{{}}}}"#
        )
    };
    let body = radius_body(&[
        row_with("no-map", ""),
        row_with("empty-map", r#""thinkingLevelMap":{},"#),
        row_with(
            "nulls-above-high",
            r#""thinkingLevelMap":{"xhigh":null,"max":null},"#,
        ),
        row_with("off-null", r#""thinkingLevelMap":{"off":null},"#),
        row_with("xhigh-named", r#""thinkingLevelMap":{"xhigh":"xhigh"},"#),
        row_with("max-named", r#""thinkingLevelMap":{"max":"max"},"#),
    ]);
    let rows = decode_radius_models(&provider, &body).expect("radius rows decode");
    let up_to_high = || {
        vec![
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ]
    };
    let with = |extra: ThinkingLevel| {
        let mut levels = up_to_high();
        levels.push(extra);
        levels
    };
    let observed: Vec<_> = rows
        .iter()
        .map(|row| (row.id.as_ref(), levels_of(row)))
        .collect();
    assert_eq!(
        observed,
        [
            ("no-map", (up_to_high(), true)),
            ("empty-map", (up_to_high(), true)),
            ("nulls-above-high", (up_to_high(), true)),
            ("off-null", (up_to_high(), false)),
            ("xhigh-named", (with(ThinkingLevel::Xhigh), true)),
            ("max-named", (with(ThinkingLevel::Max), true)),
        ],
    );
}

#[test]
fn radius_limits_keep_only_positive_values_that_fit_u32_and_drop_negative_ones() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let body = radius_body(&[
        radius_row("fits", "4294967295", "4294967295", "{}"),
        radius_row("one-over", "4294967296", "4294967296", "{}"),
        radius_row("wraps-to-one", "4294967297", "4294967297", "{}"),
        radius_row("zero", "0", "0", "{}"),
        radius_row("one", "1", "1", "{}"),
        radius_row("negative", "-1", "-1", "{}"),
    ]);
    let rows = decode_radius_models(&provider, &body).expect("radius rows decode");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_ref(), row.context_window, row.max_output))
            .collect::<Vec<_>>(),
        [
            ("fits", Some(u32::MAX), Some(u32::MAX)),
            ("one-over", None, None),
            ("wraps-to-one", None, None),
            ("zero", None, None),
            ("one", Some(1), Some(1)),
        ],
        "a negative limit drops its row; an out-of-range one is unknown, never wrapped"
    );
}

#[test]
fn radius_rates_must_be_finite_and_non_negative_or_the_row_stays_unpriced() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let mut rows_json = vec![radius_row(
        "free",
        "4096",
        "512",
        r#"{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}"#,
    )];
    for leg in ["input", "output", "cacheRead", "cacheWrite"] {
        rows_json.push(radius_row(
            &format!("negative-{leg}"),
            "4096",
            "512",
            &format!(r#"{{"{leg}":-0.5}}"#),
        ));
    }
    let rows = decode_radius_models(&provider, &radius_body(&rows_json)).expect("rows decode");
    assert_eq!(rows.len(), 5, "an invalid rate leaves the row listed");
    let free = rows[0].price.as_ref().expect("a zero-rate card is priced");
    assert_eq!(
        (free.input, free.cached_input, free.output, free.cache_write),
        (0.0, 0.0, 0.0, Some(0.0))
    );
    for row in &rows[1..] {
        assert_eq!(row.price, None, "{}", row.id);
        assert_eq!(row.context_window, Some(4096), "{}", row.id);
    }
}

#[test]
fn radius_ids_are_trimmed_and_deduplicated_after_sanitising() {
    let provider = radius_provider("https://radius.pi.dev/v1");
    let named = |id: &str, name: &str| {
        format!(
            r#"{{"id":"{id}","name":"{name}","reasoning":false,"input":["text"],"contextWindow":4096,"maxTokens":512,"cost":{{}}}}"#
        )
    };
    let body = radius_body(&[
        named("  padded  ", "First"),
        named("padded", "Second"),
        named(r"a\u0007b", "Third"),
        named(r"a\u0008b", "Fourth"),
    ]);
    let rows = decode_radius_models(&provider, &body).expect("radius rows decode");
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.as_ref(), row.display.as_ref()))
            .collect::<Vec<_>>(),
        [("padded", "First"), ("a\u{FFFD}b", "Third")],
    );
}

#[test]
fn copilot_free_models_keep_a_zero_multiplier_and_a_zero_rate_card() {
    let chat = provider(
        "github-copilot",
        Family::Chat,
        "https://api.githubcopilot.com",
    );
    let rows = decode_copilot_models(
        &chat,
        br#"{"data":[
            {"id":"free-model","name":"Free Model","capabilities":{"type":"chat","limits":{"max_context_window_tokens":128000,"max_output_tokens":16384}},"billing":{"multiplier":0,"token_prices":{"default":{"max_prompt_tokens":64000,"input_price":0,"output_price":0,"cache_price":0}}}}
        ]}"#,
    )
    .expect("copilot rows decode");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].premium_multiplier, Some(0.0));
    let price = rows[0].price.as_ref().expect("a zero-rate card is priced");
    assert_eq!(
        (price.input, price.cached_input, price.output),
        (0.0, 0.0, 0.0)
    );
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
        provider: None,
    };
    let resolved = resolve(&listed, &[], "radius/balanced").expect("a listed row resolves");
    assert_eq!(resolved.provider.as_ref(), "radius");
    assert_eq!(resolved.route.provider(), Some("radius"));
    assert!(
        resolved.route.same_model(&route),
        "the listed row keeps the requested model"
    );
    assert_eq!(resolved.entry.context_window, Some(1_048_576));
    let by_listed_route = resolve_route(&listed, &[], &route).expect("the route resolves");
    assert_eq!(by_listed_route.route.provider(), Some("radius"));
    assert!(
        by_listed_route.route.same_model(&route),
        "the provider-less route keeps the requested model"
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
        provider: None,
    };
    let resolved = resolve(&typed, &[], "radius/claude-opus-5").expect("a typed id resolves");
    assert_eq!(resolved.route.provider(), Some("radius"));
    assert!(
        resolved.route.same_model(&unlisted),
        "the typed row keeps the requested model"
    );
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
            gemini_card(
                "models/gemini-2.5-flash",
                "Gemini 2.5 Flash",
                1_048_576,
                65_536,
            ),
        ],
        Some("page-2"),
    );
    let second = gemini_page(
        &[
            gemini_card(
                "models/gemini-1.5-flash",
                "Gemini 1.5 Flash",
                1_000_000,
                8_192,
            ),
            gemini_card(
                "models/gemini-2.5-pro",
                "Gemini 2.5 Pro again",
                1_048_576,
                65_536,
            ),
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
    assert_eq!(seen[0].0, "GET /v1beta/models?pageSize=100 HTTP/1.1\r\n");
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
    let (fetched, seen) =
        gemini_refresh(&gemini_key("test-key"), vec![gemini_page(&cards, None)]).await;
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
            &[gemini_card(
                "models/gemini-2.5-pro",
                "Gemini 2.5 Pro",
                1_048_576,
                65_536,
            )],
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
    let first = gemini_page(&[gemini_card("models/gemini-a", "A", 10, 5)], Some("again"));
    let second = gemini_page(&[gemini_card("models/gemini-b", "B", 10, 5)], Some("again"));
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
                &[gemini_card(
                    &format!("models/gemini-{page:02}"),
                    "Page",
                    10,
                    5,
                )],
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
        let (fetched, seen) =
            gemini_refresh(&gemini_key("test-key"), vec![(200, body.to_vec())]).await;
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

fn query_pairs(request_line: &str) -> Vec<(String, String)> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .expect("a request line has a target");
    url::Url::parse(&format!("http://replay.test{target}"))
        .expect("the request target parses")
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

fn anthropic_page(ids: &[&str], last_id: Option<&str>, has_more: bool) -> (u16, Vec<u8>) {
    let rows = ids
        .iter()
        .map(|id| format!(r#"{{"id":"{id}"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let last = last_id.map_or_else(String::new, |last| format!(r#","last_id":"{last}""#));
    (
        200,
        format!(r#"{{"data":[{rows}],"has_more":{has_more}{last}}}"#).into_bytes(),
    )
}

/// One refresh of an Anthropic provider against `replies`, with every request
/// the scripted server saw. The server answers exactly `replies.len()`
/// requests, then closes its listener.
async fn anthropic_refresh(
    directory: &TestDir,
    replies: Vec<(u16, Vec<u8>)>,
) -> (CatalogFetch, Vec<SeenRequest>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_replies(listener, replies);
    let mut provider = provider(
        "anthropic",
        Family::Anthropic,
        &format!("http://127.0.0.1:{port}"),
    );
    provider.auth = AuthStyle::XApiKey;
    let credential = gemini_key("test-key");
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
    (fetched, server.join().expect("serve the anthropic replies"))
}

#[tokio::test]
async fn anthropic_fetch_rejects_has_more_without_a_cursor_and_caches_nothing() {
    let directory = TestDir::new();
    let (fetched, seen) =
        anthropic_refresh(&directory, vec![anthropic_page(&["claude-a"], None, true)]).await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::Anthropic,
                detail,
            }) if detail.contains("without last_id")
        ),
        "{:?}",
        fetched.live_error
    );
    assert_eq!(seen.len(), 1);
    assert!(
        !directory.path().join("models.json").exists(),
        "a failed listing is never cached"
    );
}

#[tokio::test]
async fn anthropic_fetch_rejects_a_cursor_cycle_that_is_not_the_previous_page() {
    let directory = TestDir::new();
    let (fetched, seen) = anthropic_refresh(
        &directory,
        vec![
            anthropic_page(&["claude-a"], Some("cursor-a"), true),
            anthropic_page(&["claude-b"], Some("cursor-b"), true),
            anthropic_page(&["claude-c"], Some("cursor-a"), true),
        ],
    )
    .await;
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Protocol {
                family: Family::Anthropic,
                detail,
            }) if detail.contains("repeated")
        ),
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        seen.len(),
        3,
        "the cycle is cut at the first repeated cursor"
    );
    assert_eq!(
        query_pairs(&seen[2].0),
        [
            (String::from("limit"), String::from("1000")),
            (String::from("after_id"), String::from("cursor-b")),
        ]
    );
    assert!(
        !directory.path().join("models.json").exists(),
        "a failed listing is never cached"
    );
}

#[tokio::test]
async fn anthropic_fetch_sends_a_hostile_cursor_as_one_encoded_value() {
    let directory = TestDir::new();
    let hostile = "a&limit=1#frag \u{e9}%00 +";
    let (fetched, seen) = anthropic_refresh(
        &directory,
        vec![
            anthropic_page(&["claude-a"], Some(hostile), true),
            anthropic_page(&["claude-b"], None, false),
        ],
    )
    .await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(fetched.entries.len(), 2);
    assert_eq!(
        query_pairs(&seen[1].0),
        [
            (String::from("limit"), String::from("1000")),
            (String::from("after_id"), String::from(hostile)),
        ],
        "{}",
        seen[1].0
    );
}

#[tokio::test]
async fn gemini_fetch_stops_on_a_token_cycle_that_is_not_the_previous_token() {
    let first = gemini_page(&[gemini_card("models/gemini-a", "A", 10, 5)], Some("a"));
    let second = gemini_page(&[gemini_card("models/gemini-b", "B", 10, 5)], Some("b"));
    let third = gemini_page(&[gemini_card("models/gemini-c", "C", 10, 5)], Some("a"));
    let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![first, second, third]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(gemini_ids(&fetched), ["gemini-a", "gemini-b", "gemini-c"]);
    assert_eq!(seen.len(), 3, "the revisited token `a` is not followed");
}

#[tokio::test]
async fn gemini_fetch_sends_a_hostile_page_token_as_one_encoded_value() {
    let hostile = "t&pageSize=1#frag \u{e9}%00 +";
    let first = gemini_page(&[gemini_card("models/gemini-a", "A", 10, 5)], Some(hostile));
    let second = gemini_page(&[gemini_card("models/gemini-b", "B", 10, 5)], None);
    let (fetched, seen) = gemini_refresh(&gemini_key("test-key"), vec![first, second]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        query_pairs(&seen[1].0),
        [
            (String::from("pageSize"), String::from("100")),
            (String::from("pageToken"), String::from(hostile)),
        ],
        "{}",
        seen[1].0
    );
}

#[tokio::test]
async fn gemini_limits_that_do_not_fit_u32_are_unknown_and_never_wrapped() {
    let card = |id: &str, limit: &str| {
        format!(
            r#"{{"name":"models/{id}","supportedGenerationMethods":["generateContent"],"inputTokenLimit":{limit},"outputTokenLimit":{limit}}}"#
        )
    };
    let cards = [
        card("fits", "4294967295"),
        card("one-over", "4294967296"),
        card("wraps-to-one", "4294967297"),
        card("one", "1"),
        card("zero", "0"),
        card("fraction", "1.5"),
    ];
    let (fetched, _) =
        gemini_refresh(&gemini_key("test-key"), vec![gemini_page(&cards, None)]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| (entry.id.as_ref(), entry.context_window, entry.max_output))
            .collect::<Vec<_>>(),
        [
            ("fits", Some(u32::MAX), Some(u32::MAX)),
            ("fraction", None, None),
            ("one", Some(1), Some(1)),
            ("one-over", None, None),
            ("wraps-to-one", None, None),
            ("zero", None, None),
        ],
    );
}

#[tokio::test]
async fn gemini_ids_lose_exactly_one_models_prefix_and_surrounding_whitespace() {
    let cards = [
        gemini_card("models/models/inner", "Inner", 10, 5),
        gemini_card("  models/padded  ", "Padded", 10, 5),
        gemini_card("modelsx/not-prefix", "Not prefix", 10, 5),
        gemini_card("Models/Cap", "Cap", 10, 5),
        gemini_card("models/vendor/path", "Path", 10, 5),
    ];
    let (fetched, _) =
        gemini_refresh(&gemini_key("test-key"), vec![gemini_page(&cards, None)]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        gemini_ids(&fetched),
        [
            "Models/Cap",
            "models/inner",
            "modelsx/not-prefix",
            "padded",
            "vendor/path",
        ],
    );
}

#[tokio::test]
async fn gemini_image_input_follows_each_id_hint_in_any_case_and_not_partial_hints() {
    let ids = [
        ("my-image", true),
        ("MY-IMAGE", true),
        ("VisionPro", true),
        ("GEMINI-x", true),
        ("text-only", false),
        ("imag", false),
        ("vis", false),
        ("gemin", false),
    ];
    let cards: Vec<String> = ids
        .iter()
        .map(|(id, _)| {
            format!(r#"{{"name":"models/{id}","supportedGenerationMethods":["generateContent"]}}"#)
        })
        .collect();
    let (fetched, _) =
        gemini_refresh(&gemini_key("test-key"), vec![gemini_page(&cards, None)]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(fetched.entries.len(), ids.len());
    for (id, expected) in ids {
        let entry = fetched
            .entries
            .iter()
            .find(|entry| entry.id.as_ref() == id)
            .unwrap_or_else(|| panic!("no row for {id}"));
        assert_eq!(entry.image_input, expected, "{id}");
    }
}

#[tokio::test]
async fn codex_model_fetch_sends_the_client_version_as_one_encoded_query_value() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_binary(listener, br#"{"models":[]}"#.to_vec(), "application/json");
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let credential = oauth(Some("account"));
    let client = reqwest::Client::new();
    let version = "1.2.3+beta x&y=z";
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version,
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let (request_line, _) = server.join().expect("serve the codex list");
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert!(
        request_line.starts_with("GET /backend-api/codex/models?"),
        "{request_line}"
    );
    assert_eq!(
        query_pairs(&request_line),
        [(String::from("client_version"), String::from(version))],
        "{request_line}"
    );
}

#[tokio::test]
async fn openai_compatible_fetch_gets_models_with_bearer_auth_and_a_copilot_lookalike_path_stays_generic()
 {
    for family in [Family::Chat, Family::Responses] {
        let directory = TestDir::new();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
        listener
            .set_nonblocking(true)
            .expect("make catalog listener nonblocking");
        let port = listener.local_addr().expect("read listener port").port();
        let body = br#"{"data":[{"id":"zed-model"},{"id":"gpt-6-sol"}]}"#.to_vec();
        let server = serve_binary(listener, body, "application/json");
        let provider = provider(
            "my-openai",
            family,
            &format!("http://127.0.0.1:{port}/githubcopilot.com/v1/"),
        );
        let credential = Credential::ApiKey {
            key: SecretString::from(String::from("sk-test")),
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
        let (request_line, headers) = server.join().expect("serve the model list");
        assert_eq!(
            fetched.source,
            CatalogSource::Live,
            "{family:?}: {:?}",
            fetched.live_error
        );
        assert_eq!(
            fetched
                .entries
                .iter()
                .map(|entry| entry.id.as_ref())
                .collect::<Vec<_>>(),
            ["zed-model", "gpt-6-sol"],
            "{family:?}"
        );
        assert!(
            request_line.starts_with("GET /githubcopilot.com/v1/models "),
            "{family:?}: {request_line}"
        );
        assert_eq!(
            header(&headers, "authorization"),
            Some("Bearer sk-test"),
            "{family:?}"
        );
        assert_eq!(
            header(&headers, "editor-version"),
            None,
            "{family:?}: only a githubcopilot.com host takes the Copilot path"
        );
    }
}

#[tokio::test]
async fn families_without_discovery_fall_back_typed_and_still_resolve_typed_ids() {
    for (family, id, base) in [
        (
            Family::Bedrock,
            "my-bedrock",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        ),
        (Family::Cursor, "my-cursor", "https://api.cursor.com"),
        (Family::Droid, "factory-droid", "https://api.factory.ai"),
        (Family::GitLab, "gitlab-duo", "https://gitlab.com"),
    ] {
        let directory = TestDir::new();
        let made = provider(id, family, base);
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
        assert!(
            matches!(
                &fetched.live_error,
                Some(ProviderError::InvalidRequest { message })
                    if message.contains("no catalog discovery")
            ),
            "{family:?}: {:?}",
            fetched.live_error
        );
        let catalog = Catalog::with_sources(vec![(made, fetched.source)], fetched.entries);
        let resolved = resolve(&catalog, &[], &format!("{id}/some-new-id"))
            .unwrap_or_else(|error| panic!("{family:?}: {error:?}"));
        assert_eq!(resolved.entry.context_window, None, "{family:?}");
    }
}

#[tokio::test]
async fn a_cache_write_failure_never_discards_the_live_rows() {
    let directory = TestDir::new();
    let blocker = directory.path().join("not-a-directory");
    fs::write(&blocker, b"plain file").expect("write the blocking file");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let body = br#"{"data":[{"id":"mistral-live","max_context_length":1000}]}"#.to_vec();
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
        cache_dir: &blocker,
        user_agent: "dalgon/test",
        version: "test",
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    server.join().expect("serve the mistral list");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert!(fetched.live_error.is_none(), "{:?}", fetched.live_error);
    assert_eq!(fetched.entries.len(), 1);
    assert_eq!(fetched.entries[0].id.as_ref(), "mistral-live");
    assert!(
        fetched
            .cache_error
            .as_ref()
            .is_some_and(|error| error.contains("cache I/O failed")),
        "{:?}",
        fetched.cache_error
    );
}

#[tokio::test]
async fn a_live_refresh_over_a_corrupt_cache_succeeds_reports_the_damage_and_repairs_the_file() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    fs::write(&cache_path, b"{ not json").expect("write the corrupt cache");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let body = br#"{"data":[{"id":"mistral-live","max_context_length":1000}]}"#.to_vec();
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
    server.join().expect("serve the mistral list");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(fetched.entries.len(), 1);
    assert!(
        fetched
            .cache_error
            .as_ref()
            .is_some_and(|error| error.contains("models cache JSON is invalid")),
        "the damaged cache is reported, not silently replaced: {:?}",
        fetched.cache_error
    );
    let repaired = read_cache_async(cache_path)
        .await
        .expect("the fresh rows replaced the corrupt file");
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0].id.as_ref(), "mistral-live");
}

#[tokio::test]
async fn a_refresh_replaces_the_providers_old_rows_and_keeps_every_other_providers() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let names = |entries: &[CatalogEntry]| {
        entries
            .iter()
            .map(|entry| format!("{}/{}", entry.provider, entry.id))
            .collect::<Vec<_>>()
    };
    write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![
            row("anthropic", "a1", Some(1)),
            row("anthropic", "a2", Some(1)),
        ],
    )
    .await
    .expect("write the first anthropic rows");
    write_cache_async(
        cache_path.clone(),
        "openai".into(),
        vec![row("openai", "o1", Some(1))],
    )
    .await
    .expect("write the openai row");
    write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![row("anthropic", "a3", Some(1))],
    )
    .await
    .expect("refresh the anthropic rows");
    let entries = read_cache_async(cache_path.clone())
        .await
        .expect("read the merged cache");
    assert_eq!(
        names(&entries),
        ["openai/o1", "anthropic/a3"],
        "removed upstream models must not survive a refresh"
    );
    write_cache_async(cache_path.clone(), "openai".into(), Vec::new())
        .await
        .expect("refresh openai to an empty list");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read the cache after the empty refresh");
    assert_eq!(names(&entries), ["anthropic/a3"]);
}

#[tokio::test]
async fn a_corrupt_or_unsupported_cache_is_replaced_by_the_fresh_rows_and_the_damage_is_reported() {
    for damaged in [
        &b"{ not json"[..],
        br#"{"version":1,"entries":[{"provider":"anth"#,
    ] {
        let directory = TestDir::new();
        let cache_path = directory.path().join("models.json");
        fs::write(&cache_path, damaged).expect("write the damaged cache");
        let outcome = write_cache_async(
            cache_path.clone(),
            "anthropic".into(),
            vec![row("anthropic", "fresh", Some(7))],
        )
        .await;
        assert!(
            matches!(&outcome, Err(CacheError::Json(_))),
            "{}: {outcome:?}",
            String::from_utf8_lossy(damaged)
        );
        let repaired = read_cache_async(cache_path)
            .await
            .expect("the fresh rows replaced the damaged file");
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].id.as_ref(), "fresh");
        assert_eq!(repaired[0].context_window, Some(7));
    }

    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    fs::write(&cache_path, br#"{"version":9,"entries":[]}"#).expect("write the future cache");
    let outcome = write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![row("anthropic", "fresh", Some(7))],
    )
    .await;
    assert!(
        matches!(&outcome, Err(CacheError::Version(9))),
        "{outcome:?}"
    );
    assert_eq!(
        read_cache_async(cache_path)
            .await
            .expect("the fresh rows replaced the future file")
            .len(),
        1
    );
}

#[tokio::test]
async fn only_cache_version_one_is_read_and_every_other_version_is_rejected_with_its_number() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    for version in [0_u32, 1, 2, u32::MAX] {
        fs::write(
            &cache_path,
            format!(r#"{{"version":{version},"entries":[]}}"#),
        )
        .expect("write the versioned cache");
        let outcome = read_cache_async(cache_path.clone()).await;
        if version == 1 {
            assert!(
                matches!(&outcome, Ok(entries) if entries.is_empty()),
                "{outcome:?}"
            );
        } else {
            assert!(
                matches!(&outcome, Err(CacheError::Version(found)) if *found == version),
                "version {version}: {outcome:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cached_row_with_any_empty_identity_field_is_invalid() {
    let cases: [(&str, EntryEdit); 3] = [
        ("provider", |entry| entry.provider = Box::from("")),
        ("id", |entry| entry.id = Box::from("")),
        ("display", |entry| entry.display = Box::from("")),
    ];
    for (field, blank) in cases {
        let directory = TestDir::new();
        let cache_path = directory.path().join("models.json");
        let mut entry = row("anthropic", "model", Some(1));
        blank(&mut entry);
        write_cache_async(cache_path.clone(), "anthropic".into(), vec![entry])
            .await
            .expect("encoding does not validate the row");
        let outcome = read_cache_async(cache_path).await;
        assert!(
            matches!(
                &outcome,
                Err(CacheError::Invalid(message))
                    if message.contains("empty provider, id, or display name")
            ),
            "empty {field}: {outcome:?}"
        );
    }
}

/// An in-place edit of one cached row, used to spoil a single field.
type EntryEdit = fn(&mut CatalogEntry);

fn flat_price() -> ModelPrice {
    ModelPrice {
        input: 1.0,
        cached_input: 0.5,
        output: 2.0,
        reasoning: 2.0,
        cache_write: Some(1.25),
        tiers: Box::default(),
    }
}

fn price_tier(set: impl FnOnce(&mut dal_core::PriceTier)) -> Box<[dal_core::PriceTier]> {
    let mut tier = dal_core::PriceTier {
        size: 100,
        input: None,
        cached_input: None,
        cache_write: None,
        output: None,
        reasoning: None,
    };
    set(&mut tier);
    vec![tier].into_boxed_slice()
}

#[tokio::test]
async fn a_free_model_cached_with_zero_rates_and_a_zero_multiplier_is_valid() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut free = row("anthropic", "free", Some(1));
    free.premium_multiplier = Some(0.0);
    free.price = Some(ModelPrice {
        input: 0.0,
        cached_input: 0.0,
        output: 0.0,
        reasoning: 0.0,
        cache_write: Some(0.0),
        tiers: price_tier(|tier| {
            tier.input = Some(0.0);
            tier.cached_input = Some(0.0);
            tier.cache_write = Some(0.0);
            tier.output = Some(0.0);
            tier.reasoning = Some(0.0);
        }),
    });
    write_cache_async(cache_path.clone(), "anthropic".into(), vec![free.clone()])
        .await
        .expect("write the free row");
    let entries = read_cache_async(cache_path)
        .await
        .expect("a zero rate is a valid rate");
    assert_eq!(entries[0].premium_multiplier, Some(0.0));
    assert_eq!(entries[0].price, free.price);
}

#[tokio::test]
async fn a_cached_row_with_any_negative_rate_leg_or_multiplier_is_invalid() {
    let cases: [(&str, EntryEdit); 11] = [
        ("input", |entry| {
            entry.price = Some(ModelPrice {
                input: -0.5,
                ..flat_price()
            });
        }),
        ("cached_input", |entry| {
            entry.price = Some(ModelPrice {
                cached_input: -0.5,
                ..flat_price()
            });
        }),
        ("output", |entry| {
            entry.price = Some(ModelPrice {
                output: -0.5,
                ..flat_price()
            });
        }),
        ("reasoning", |entry| {
            entry.price = Some(ModelPrice {
                reasoning: -0.5,
                ..flat_price()
            });
        }),
        ("cache_write", |entry| {
            entry.price = Some(ModelPrice {
                cache_write: Some(-0.5),
                ..flat_price()
            });
        }),
        ("tier input", |entry| {
            entry.price = Some(ModelPrice {
                tiers: price_tier(|tier| tier.input = Some(-0.5)),
                ..flat_price()
            });
        }),
        ("tier cached_input", |entry| {
            entry.price = Some(ModelPrice {
                tiers: price_tier(|tier| tier.cached_input = Some(-0.5)),
                ..flat_price()
            });
        }),
        ("tier cache_write", |entry| {
            entry.price = Some(ModelPrice {
                tiers: price_tier(|tier| tier.cache_write = Some(-0.5)),
                ..flat_price()
            });
        }),
        ("tier output", |entry| {
            entry.price = Some(ModelPrice {
                tiers: price_tier(|tier| tier.output = Some(-0.5)),
                ..flat_price()
            });
        }),
        ("tier reasoning", |entry| {
            entry.price = Some(ModelPrice {
                tiers: price_tier(|tier| tier.reasoning = Some(-0.5)),
                ..flat_price()
            });
        }),
        ("premium multiplier", |entry| {
            entry.premium_multiplier = Some(-0.5);
        }),
    ];
    for (leg, spoil) in cases {
        let directory = TestDir::new();
        let cache_path = directory.path().join("models.json");
        let mut entry = row("anthropic", "model", Some(1));
        spoil(&mut entry);
        write_cache_async(cache_path.clone(), "anthropic".into(), vec![entry])
            .await
            .expect("encoding does not validate the row");
        let expected = if leg == "premium multiplier" {
            "invalid premium multiplier"
        } else {
            "invalid model price"
        };
        let outcome = read_cache_async(cache_path).await;
        assert!(
            matches!(&outcome, Err(CacheError::Invalid(message)) if message.contains(expected)),
            "negative {leg}: {outcome:?}"
        );
    }
}

#[tokio::test]
async fn every_tool_support_variant_survives_the_cache_unchanged() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let variants = [
        ToolSupport::None,
        ToolSupport::Any,
        ToolSupport::ResponsesOnly,
        ToolSupport::ChatWhenNoReasoning,
    ];
    let rows: Vec<CatalogEntry> = variants
        .iter()
        .enumerate()
        .map(|(index, support)| {
            let mut entry = row("anthropic", &format!("tools-{index}"), Some(1));
            entry.tool_support = *support;
            entry
        })
        .collect();
    write_cache_async(cache_path.clone(), "anthropic".into(), rows)
        .await
        .expect("write the tool-support rows");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read the tool-support rows");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.tool_support)
            .collect::<Vec<_>>(),
        variants,
    );
}

#[tokio::test]
async fn every_thinking_variant_survives_the_cache_and_unknown_efforts_are_dropped() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let variants = [
        ThinkingSupport::OpenAi {
            accepted: vec![ThinkingLevel::Low, ThinkingLevel::Xhigh, ThinkingLevel::Max],
            none_supported: true,
        },
        ThinkingSupport::OpenAi {
            accepted: Vec::new(),
            none_supported: false,
        },
        ThinkingSupport::Adaptive {
            can_disable: true,
            accepted: vec![
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max,
            ],
        },
        ThinkingSupport::Adaptive {
            can_disable: false,
            accepted: vec![Effort::Max, Effort::Low],
        },
        ThinkingSupport::Budget { can_disable: true },
        ThinkingSupport::Budget { can_disable: false },
        ThinkingSupport::UnknownAdaptive,
    ];
    let rows: Vec<CatalogEntry> = variants
        .iter()
        .enumerate()
        .map(|(index, thinking)| {
            let mut entry = row("anthropic", &format!("thinking-{index}"), Some(1));
            entry.thinking = thinking.clone();
            entry
        })
        .collect();
    write_cache_async(cache_path.clone(), "anthropic".into(), rows)
        .await
        .expect("write the thinking rows");
    let entries = read_cache_async(cache_path.clone())
        .await
        .expect("read the thinking rows");
    assert_eq!(entries.len(), variants.len());
    for (entry, expected) in entries.iter().zip(&variants) {
        assert_eq!(
            format!("{:?}", entry.thinking),
            format!("{expected:?}"),
            "{}",
            entry.id
        );
    }

    fs::write(
        &cache_path,
        br#"{"version":1,"entries":[{"provider":"anthropic","id":"m","display":"m","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"adaptive","can_disable":true,"accepted":["low","bogus","max",""]},"image_input":false,"remote_compact":false,"billing":{"premium_multiplier":null,"price":null}}]}"#,
    )
    .expect("write the hand-edited cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read the hand-edited cache");
    assert_eq!(
        format!("{:?}", entries[0].thinking),
        format!(
            "{:?}",
            ThinkingSupport::Adaptive {
                can_disable: true,
                accepted: vec![Effort::Low, Effort::Max],
            }
        ),
        "an effort this build does not know is dropped, the rest keep their order"
    );
}

#[tokio::test]
async fn the_cache_keeps_hidden_listing_and_every_scalar_capability_field() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut loud = row("anthropic", "loud", Some(111));
    loud.display = "Loud Name".into();
    loud.listing = Listing::Hidden;
    loud.max_output = Some(222);
    loud.image_input = true;
    loud.remote_compact = true;
    loud.supports_reasoning_summaries = true;
    loud.custom_grammar = true;
    loud.temperature_allowed = true;
    loud.display_supported = true;
    let quiet = row("anthropic", "quiet", None);
    write_cache_async(cache_path.clone(), "anthropic".into(), vec![loud, quiet])
        .await
        .expect("write the capability rows");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read the capability rows");
    let (loud, quiet) = (&entries[0], &entries[1]);
    assert_eq!(loud.display.as_ref(), "Loud Name");
    assert_eq!(loud.listing, Listing::Hidden);
    assert_eq!(loud.context_window, Some(111));
    assert_eq!(loud.max_output, Some(222));
    assert!(loud.image_input);
    assert!(loud.remote_compact);
    assert!(loud.supports_reasoning_summaries);
    assert!(loud.custom_grammar);
    assert!(loud.temperature_allowed);
    assert!(loud.display_supported);
    assert_eq!(quiet.listing, Listing::Listed);
    assert_eq!(quiet.context_window, None);
    assert_eq!(quiet.max_output, None);
    assert!(!quiet.image_input);
    assert!(!quiet.remote_compact);
    assert!(!quiet.supports_reasoning_summaries);
    assert!(!quiet.custom_grammar);
    assert!(!quiet.temperature_allowed);
    assert!(!quiet.display_supported);
}

#[test]
fn tier_guard_agrees_with_str_ordering_for_prefixes_case_and_multibyte_ids() {
    // `binary_search_by_key` orders by `str`, so the guard that vouches for the
    // generated table must accept exactly the strictly increasing `str` pairs.
    let corpus = [
        "",
        "a",
        "ab",
        "abc",
        "b",
        "B",
        "Z",
        "\u{e9}",
        "\u{65e5}\u{672c}",
        "a/b",
        "a-b",
        "a.b",
    ];
    for left in corpus {
        for right in corpus {
            let rows = [
                TierRow {
                    model: left,
                    tiers: &[],
                },
                TierRow {
                    model: right,
                    tiers: &[],
                },
            ];
            assert_eq!(
                tier_rows_are_sorted_unique(&rows),
                left < right,
                "{left:?} then {right:?}"
            );
        }
    }
    assert!(tier_rows_are_sorted_unique(&[]));
    assert!(tier_rows_are_sorted_unique(&[TierRow {
        model: "only",
        tiers: &[],
    }]));
}

#[test]
fn compiled_price_maps_every_snapshot_rate_to_its_own_field() {
    let rates = |row: &PriceRow| {
        [
            row.input,
            row.cached_input,
            row.cache_write,
            row.output,
            row.reasoning,
        ]
    };
    let full = PRICE_ROWS
        .iter()
        .find(|row| {
            let present: Vec<f64> = rates(row).into_iter().flatten().collect();
            present.len() == 5
                && present.iter().enumerate().all(|(index, rate)| {
                    present[index + 1..]
                        .iter()
                        .all(|other| other.to_bits() != rate.to_bits())
                })
        })
        .expect("the snapshot prices a model with five distinct rates");
    let price = compiled_price(full.model).expect("the snapshot model resolves");
    assert_eq!(
        (
            Some(price.input),
            Some(price.cached_input),
            price.cache_write,
            Some(price.output),
            Some(price.reasoning),
        ),
        (
            full.input,
            full.cached_input,
            full.cache_write,
            full.output,
            full.reasoning,
        ),
        "{}",
        full.model
    );
}

#[test]
fn compiled_price_leaves_a_missing_cache_write_unknown_so_cost_falls_back_to_input() {
    use dal_core::Usage;

    let row = PRICE_ROWS
        .iter()
        .find(|row| row.cache_write.is_none() && row.input.is_some_and(|rate| rate > 0.0))
        .expect("the snapshot has a priced row without a cache-write rate");
    let price = compiled_price(row.model).expect("the snapshot model resolves");
    assert_eq!(price.cache_write, None, "{}", row.model);
    let usage = Usage {
        input_tokens: 1_000_000,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 1_000_000,
        cost_usd: None,
        premium_requests: None,
    };
    let explicit = ModelPrice {
        cache_write: Some(price.input),
        ..price.clone()
    };
    let cost = usage.cost_usd(None, Some(&price));
    assert!(
        cost.is_some_and(|cost| cost > 0.0),
        "{}: {cost:?}",
        row.model
    );
    assert_eq!(
        cost,
        usage.cost_usd(None, Some(&explicit)),
        "{}: cache writes bill at the input rate",
        row.model
    );
}
