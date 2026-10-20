use std::collections::BTreeMap;
use std::path::PathBuf;

use dal_core::{ApprovalMode, Caps, Family, ModelInfo, ModelRoute};

use crate::router::decode::{ResolvedModel, listed_ids, resolve_model, route_id};
use crate::router::{HarnessMode, RouterOptions};

fn options(aliases: &[(&str, &str)]) -> RouterOptions {
    RouterOptions {
        bind: "127.0.0.1".to_owned(),
        port: 0,
        public: false,
        a2a: false,
        token_file: PathBuf::new(),
        approval: ApprovalMode::Ask,
        origins: Vec::new(),
        aliases: aliases
            .iter()
            .map(|(alias, target)| ((*alias).into(), (*target).into()))
            .collect::<BTreeMap<Box<str>, Box<str>>>(),
        workspace: PathBuf::from("/"),
    }
}

/// One catalog row as `Host::models` produces it: an API route names the
/// provider that listed it, and `provider` repeats that identity.
fn info(route: ModelRoute) -> ModelInfo {
    ModelInfo {
        provider: route.provider().unwrap_or_default().into(),
        route,
        name: "model".into(),
        caps: Caps {
            context_window: None,
            thinking: Box::new([]),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
    }
}

fn api(family: Family, model: &str, provider: &str) -> ModelRoute {
    ModelRoute::Api {
        family,
        model: model.into(),
        provider: Some(provider.into()),
    }
}

fn catalog() -> Vec<ModelInfo> {
    vec![
        info(api(Family::Chat, "gpt-4o", "acme-proxy")),
        info(api(Family::Responses, "o3", "openai")),
        info(api(Family::Codex, "gpt-5-codex", "openai-codex")),
        info(api(Family::Anthropic, "claude-sonnet-4", "anthropic")),
        info(api(Family::Chat, "org/nested-model", "acme-proxy")),
        info(ModelRoute::Synthetic {
            id: "team/fast".into(),
        }),
        info(ModelRoute::Harness {
            id: "dalgon/normal".into(),
        }),
        info(api(Family::Chat, "gpt-4o", "openai")),
    ]
}

#[test]
fn listed_ids_are_harness_then_aliases_then_sorted_catalog() {
    let options = options(&[("quick", "dalgon/eval-only"), ("fast", "team/fast")]);
    assert_eq!(
        listed_ids(&options.aliases, &catalog()),
        [
            "dalgon/normal",
            "dalgon/eval-first",
            "dalgon/eval-only",
            "fast",
            "quick",
            "anthropic/claude-sonnet-4",
            "openai-chat/gpt-4o",
            "openai-chat/org/nested-model",
            "openai-codex/gpt-5-codex",
            "openai-responses/o3",
            "team/fast",
        ]
    );
}

#[test]
fn every_listed_id_resolves_to_its_own_identity() {
    let options = options(&[("quick", "dalgon/eval-only"), ("fast", "team/fast")]);
    let catalog = catalog();
    for id in listed_ids(&options.aliases, &catalog) {
        let resolved = resolve_model(&options, &id).expect("listed id resolves");
        if let Some(target) = options.aliases.get(id.as_str()) {
            let through = resolve_model(&options, target).expect("alias target resolves");
            assert_eq!(
                format!("{resolved:?}"),
                format!("{through:?}"),
                "alias {id} diverges from its target {target}"
            );
        } else if let Some(mode) = HarnessMode::parse(&id) {
            let ResolvedModel::Harness(found) = resolved else {
                panic!("harness id {id} resolved away from the harness: {resolved:?}");
            };
            assert_eq!(found, mode, "harness id {id} resolved to the wrong mode");
        } else {
            let entry = catalog
                .iter()
                .find(|entry| route_id(&entry.route) == id)
                .expect("listed id is a harness id, an alias, or a catalog route");
            let ResolvedModel::Route(route) = resolved else {
                panic!("catalog id {id} resolved away from its route: {resolved:?}");
            };
            assert!(
                route.same_model(&entry.route),
                "catalog id {id} resolved to the wrong route {route:?}"
            );
            assert_eq!(
                route.provider(),
                None,
                "the router id namespace names no provider"
            );
        }
    }
}

#[test]
fn catalog_ids_round_trip_to_their_routes() {
    let options = options(&[]);
    for entry in catalog() {
        let resolved =
            resolve_model(&options, &route_id(&entry.route)).expect("catalog id resolves");
        match (resolved, &entry.route) {
            (ResolvedModel::Harness(mode), ModelRoute::Harness { id }) => {
                assert_eq!(mode.id(), id.as_ref());
            }
            (ResolvedModel::Route(route), expected) => {
                assert!(
                    route.same_model(expected),
                    "{expected:?} resolved to {route:?}"
                );
                assert_eq!(route.provider(), None, "{expected:?} gained a provider");
            }
            (other, expected) => panic!("{expected:?} resolved to {other:?}"),
        }
    }
}

#[test]
fn providers_sharing_a_family_and_model_list_one_router_id_naming_no_provider() {
    let options = options(&[]);
    let catalog = [
        info(api(Family::Chat, "gpt-4o", "openai")),
        info(api(Family::Chat, "gpt-4o", "acme-proxy")),
    ];
    let ids = listed_ids(&BTreeMap::new(), &catalog);
    assert_eq!(
        ids.iter().filter(|id| *id == "openai-chat/gpt-4o").count(),
        1,
        "both providers list the one id: {ids:?}"
    );
    let resolved = resolve_model(&options, "openai-chat/gpt-4o").expect("the listed id resolves");
    let ResolvedModel::Route(route) = resolved else {
        panic!("an API id resolved to {resolved:?}");
    };
    assert_eq!(route, api_without_provider(Family::Chat, "gpt-4o"));
    // A route that names no provider is a different selection by `==` but
    // the same model for both providers' catalog rows.
    for entry in &catalog {
        assert_ne!(route, entry.route);
        assert!(route.same_model(&entry.route));
    }
}

fn api_without_provider(family: Family, model: &str) -> ModelRoute {
    ModelRoute::Api {
        family,
        model: model.into(),
        provider: None,
    }
}

#[test]
fn aliases_resolve_one_level() {
    let options = options(&[("quick", "dalgon/eval-only"), ("fast", "team/fast")]);
    assert!(matches!(
        resolve_model(&options, "quick"),
        Ok(ResolvedModel::Harness(HarnessMode::EvalOnly))
    ));
    assert!(matches!(
        resolve_model(&options, "fast"),
        Ok(ResolvedModel::Route(ModelRoute::Synthetic { id })) if id.as_ref() == "team/fast"
    ));
}

#[test]
fn unlisted_spellings_are_model_not_found() {
    let options = options(&[]);
    for id in ["gpt-4o", "openai-chat/", "Team/Fast", "dalgon/fast"] {
        let fail = resolve_model(&options, id).expect_err("unknown id is refused");
        assert_eq!(fail.status, 400);
        assert_eq!(fail.code, "model_not_found");
        assert_eq!(fail.message, format!(r#"model "{id}" was not found"#));
    }
}

#[test]
fn ids_that_would_resolve_elsewhere_are_not_listed() {
    let colliding = [info(ModelRoute::Synthetic {
        id: "anthropic/x".into(),
    })];
    assert_eq!(
        listed_ids(&BTreeMap::new(), &colliding),
        ["dalgon/normal", "dalgon/eval-first", "dalgon/eval-only"]
    );
}
