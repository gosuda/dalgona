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

fn info(route: ModelRoute) -> ModelInfo {
    ModelInfo {
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

fn api(family: Family, model: &str) -> ModelRoute {
    ModelRoute::Api {
        family,
        model: model.into(),
    }
}

fn catalog() -> Vec<ModelInfo> {
    vec![
        info(api(Family::Chat, "gpt-4o")),
        info(api(Family::Responses, "o3")),
        info(api(Family::Codex, "gpt-5-codex")),
        info(api(Family::Anthropic, "claude-sonnet-4")),
        info(api(Family::Chat, "org/nested-model")),
        info(ModelRoute::Synthetic {
            id: "team/fast".into(),
        }),
        info(ModelRoute::Harness {
            id: "dalgon/normal".into(),
        }),
        info(api(Family::Chat, "gpt-4o")),
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
fn every_listed_id_resolves() {
    let options = options(&[("quick", "dalgon/eval-only"), ("fast", "team/fast")]);
    for id in listed_ids(&options.aliases, &catalog()) {
        assert!(
            resolve_model(&options, &id).is_ok(),
            "listed id {id} does not resolve"
        );
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
            (ResolvedModel::Route(route), expected) => assert_eq!(&route, expected),
            (other, expected) => panic!("{expected:?} resolved to {other:?}"),
        }
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
