//! Overlay staging, turn-boundary publication, promotion, and drop rules.

use std::collections::BTreeSet;
use std::sync::Arc;

use dal_core::ext::{McpBlock, McpServerDecl};
use dal_core::{
    Caps, Mode, ModelId, ModelInfo, ModelRoute, Name, Origin, RawJson, ServiceSet, SkillRecord,
    ToolClass, ToolSpec, Visibility, Workspace,
};

use super::{Overlay, TurnTools};
use crate::error::ServiceError;
use crate::ext::generation::{Generation, ValidatedExtensions};
use crate::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use crate::ext::{BoxFuture, ExtensionBuilder};
use crate::session::context::tool_list;

struct Stub {
    name: Name,
    declaring: Option<Name>,
    spec: Arc<ToolSpec>,
}

impl Tool for Stub {
    fn declaring_extension(&self) -> Option<Name> {
        self.declaring.clone()
    }

    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Interrupted })
    }
}

fn stub(name: &str) -> Arc<dyn Tool> {
    stub_declared(name, None)
}

fn stub_declared(name: &str, declaring: Option<&str>) -> Arc<dyn Tool> {
    let name = Name::parse_mapped_tool(name).unwrap();
    let declaring = declaring.map(|name| name.parse().unwrap());
    Arc::new(Stub {
        spec: Arc::new(ToolSpec {
            name: name.clone(),
            description: "stub".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).unwrap(),
            grammar: None,
        }),
        name,
        declaring,
    })
}

fn extension(name: &str, tools: &[&str]) -> crate::ext::Extension {
    let mut builder = ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
        .unwrap()
        .with_origin(Origin::Builtin, None);
    for tool in tools {
        builder = builder.tool(stub(tool), Visibility::Model);
    }
    builder.build().unwrap()
}

fn generation(extensions: Vec<crate::ext::Extension>) -> Generation {
    Generation::build(ValidatedExtensions::validate(extensions, None).unwrap())
}

fn model() -> ModelInfo {
    ModelInfo {
        route: ModelRoute::from_id("acme/test"),
        name: "test-model".into(),
        caps: Caps {
            context_window: Some(8192),
            thinking: Box::default(),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
    }
}

fn listed(generation: &Generation, tools: &TurnTools) -> Vec<String> {
    let model_id = ModelId::parse("acme/test").unwrap();
    let (specs, _) = tool_list(generation, tools, &model(), &model_id, Mode::Normal);
    let mut names: Vec<String> = specs
        .into_iter()
        .map(|spec| spec.name.to_string())
        .collect();
    names.sort();
    names
}

fn deferred_names(generation: &Generation, tools: &TurnTools) -> Vec<String> {
    let model_id = ModelId::parse("acme/test").unwrap();
    let (_, deferred) = tool_list(generation, tools, &model(), &model_id, Mode::Normal);
    deferred.iter().map(|tool| tool.name.to_string()).collect()
}

fn name(text: &str) -> Name {
    Name::parse_mapped_tool(text).unwrap()
}

#[test]
fn deferred_overlay_tool_lists_only_after_promotion_at_a_turn_boundary() {
    let generation = generation(vec![
        extension("core", &["native"]),
        extension("battery", &[]),
    ]);
    let overlay = Overlay::default();
    let none = Arc::new(BTreeSet::new());
    let running = overlay.publish(&generation, Arc::clone(&none));

    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(stub("mcp_web"), Visibility::Deferred)],
        )
        .unwrap();
    assert_eq!(
        listed(&generation, &running),
        ["native"],
        "a turn already running keeps the tool bytes it started with"
    );

    let next = overlay.publish(&generation, Arc::clone(&none));
    assert_eq!(
        listed(&generation, &next),
        ["native", "tool_search"],
        "tool_search is the only model-visible path to a deferred tool"
    );
    assert_eq!(deferred_names(&generation, &next), ["mcp_web"]);
    let model_id = ModelId::parse("acme/test").unwrap();
    let (specs, _) = tool_list(&generation, &next, &model(), &model_id, Mode::Normal);
    assert_eq!(
        specs.last().map(|spec| spec.name.as_ref()),
        Some("tool_search")
    );
    let (_, declared) = next
        .tool(&generation, &name("mcp_web"))
        .expect("a deferred overlay tool still resolves for its first call");
    assert_eq!(declared, Visibility::Deferred);

    let promoted = Arc::new(BTreeSet::from([name("mcp_web")]));
    let after = overlay.publish(&generation, promoted);
    assert_eq!(listed(&generation, &after), ["mcp_web", "native"]);
    assert!(deferred_names(&generation, &after).is_empty());
    assert_eq!(
        listed(&generation, &next),
        ["native", "tool_search"],
        "the earlier turn's snapshot never gains the promoted tool"
    );
}

#[test]
fn model_visible_overlay_tools_reach_the_next_turn_only() {
    let generation = generation(vec![extension("battery", &[])]);
    let overlay = Overlay::default();
    let none = Arc::new(BTreeSet::new());
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(stub("mcp_now"), Visibility::Model)],
        )
        .unwrap();
    let staged_first = overlay.publish(&generation, Arc::clone(&none));
    assert_eq!(listed(&generation, &staged_first), ["mcp_now"]);
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(stub("mcp_later"), Visibility::Model)],
        )
        .unwrap();
    assert_eq!(
        listed(&generation, &staged_first),
        ["mcp_now"],
        "replacing the set changes no published snapshot"
    );
    assert_eq!(
        listed(&generation, &overlay.publish(&generation, none)),
        ["mcp_later"],
        "the owner's set is replaced, not merged"
    );
}

#[test]
fn staging_rejects_taken_names_and_registers_nothing() {
    let generation = generation(vec![
        extension("core", &["native"]),
        extension("battery", &[]),
        extension("other", &[]),
    ]);
    let overlay = Overlay::default();
    let battery = name("battery");
    overlay
        .stage(
            &generation,
            &battery,
            vec![(stub("held"), Visibility::Model)],
        )
        .unwrap();

    let taken = |error: ServiceError| match error {
        ServiceError::ToolNameInUse { name, held_by } => (name.to_string(), held_by.to_string()),
        other => panic!("expected ToolNameInUse, got {other:?}"),
    };
    let generation_clash = overlay
        .stage(
            &generation,
            &name("other"),
            vec![
                (stub("fresh"), Visibility::Model),
                (stub("native"), Visibility::Model),
            ],
        )
        .unwrap_err();
    assert_eq!(taken(generation_clash), ("native".into(), "core".into()));
    let owner_clash = overlay
        .stage(
            &generation,
            &name("other"),
            vec![(stub("held"), Visibility::Model)],
        )
        .unwrap_err();
    assert_eq!(taken(owner_clash), ("held".into(), "battery".into()));
    let batch_clash = overlay
        .stage(
            &generation,
            &name("other"),
            vec![
                (stub("twin"), Visibility::Model),
                (stub("twin"), Visibility::Deferred),
            ],
        )
        .unwrap_err();
    assert_eq!(taken(batch_clash), ("twin".into(), "other".into()));

    let none = Arc::new(BTreeSet::new());
    assert_eq!(
        listed(
            &generation,
            &overlay.publish(&generation, Arc::clone(&none))
        ),
        ["held", "native"],
        "a rejected batch stages none of its tools"
    );
    overlay.stage(&generation, &battery, Vec::new()).unwrap();
    assert_eq!(
        listed(&generation, &overlay.publish(&generation, none)),
        ["native"],
        "an empty set clears the owner"
    );
}

#[test]
fn entries_drop_with_their_owner_and_lose_to_generation_names() {
    let with_owner = generation(vec![extension("battery", &[])]);
    let overlay = Overlay::default();
    let none = Arc::new(BTreeSet::new());
    overlay
        .stage(
            &with_owner,
            &name("battery"),
            vec![(stub("mcp_web"), Visibility::Model)],
        )
        .unwrap();

    let claimed = generation(vec![
        extension("battery", &[]),
        extension("core", &["mcp_web"]),
    ]);
    assert_eq!(
        listed(&claimed, &overlay.publish(&claimed, Arc::clone(&none))),
        ["mcp_web"],
        "a name the generation now claims resolves to the generation's tool alone"
    );
    assert!(
        overlay
            .publish(&claimed, Arc::clone(&none))
            .owner(&name("mcp_web"))
            .is_none()
    );

    let without_owner = generation(vec![extension("core", &["native"])]);
    assert_eq!(
        listed(
            &without_owner,
            &overlay.publish(&without_owner, Arc::clone(&none))
        ),
        ["native"]
    );
    assert!(
        overlay.publish(&with_owner, none).entries().is_empty(),
        "an owner that left the generation is dropped for good"
    );
}

fn docs_skill(web_url: &str) -> SkillRecord {
    SkillRecord {
        name: name("docs"),
        description: "Documentation".into(),
        body: "Read documentation".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: [
                (
                    "api".into(),
                    McpServerDecl::Http {
                        url: "https://api.example/mcp".into(),
                    },
                ),
                (
                    "cli".into(),
                    McpServerDecl::Stdio {
                        command: vec!["mcp".into(), "--config".into(), "/etc/mcp.json".into()],
                        env: std::collections::BTreeMap::new(),
                    },
                ),
                (
                    "web".into(),
                    McpServerDecl::Http {
                        url: web_url.into(),
                    },
                ),
            ]
            .into(),
        }),
    }
}

fn docs_extension(web_url: &str, mcp: ServiceSet) -> crate::ext::Extension {
    ExtensionBuilder::new("docs", "1.0.0", mcp)
        .unwrap()
        .with_origin(Origin::User, None)
        .skill(docs_skill(web_url))
        .build()
        .unwrap()
}

#[test]
fn overlay_accepts_entry_and_folded_mcp_tool_names() {
    let mcp = ServiceSet::from_names(["mcp"]).unwrap();
    let declaring = docs_extension("https://docs.example/mcp", mcp);
    let base_generation = generation(vec![extension("battery", &[]), declaring]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &base_generation,
            &name("battery"),
            vec![
                (
                    stub_declared("docs.web.", Some("docs")),
                    Visibility::Deferred,
                ),
                (
                    stub_declared("docs.web.list", Some("docs")),
                    Visibility::Deferred,
                ),
                (
                    stub_declared("docs.web.x.list", Some("docs")),
                    Visibility::Deferred,
                ),
            ],
        )
        .unwrap();
    let tools = overlay.publish(&base_generation, Arc::new(BTreeSet::new()));
    let docs = name("docs");
    assert_eq!(tools.owner(&name("docs.web.list")), Some(&docs));
    assert_eq!(tools.owner(&name("docs.web.")), Some(&docs));
    assert_eq!(tools.owner(&name("docs.web.x.list")), Some(&docs));
    let declared = tools
        .declared_mcp(&name("docs.web.list"))
        .expect("mapped tool preserves its declaration");
    assert_eq!(declared.plugin, docs);
    assert_eq!(declared.skill, name("docs"));
    assert!(declared.block.servers.contains_key("web"));
    assert!(
        tools
            .declared_mcp(&name("docs.web."))
            .is_some_and(|entry| entry.block.servers.contains_key("web"))
    );
    assert!(
        tools
            .declared_mcp(&name("docs.web.x.list"))
            .is_some_and(|entry| entry.block.servers.contains_key("web"))
    );
    let (set, detail) = super::mcp_grant_request(&base_generation, &docs).unwrap();
    assert_eq!(
        detail.as_ref(),
        "docs.api: URL https://api.example/mcp\ndocs.cli: command [\"mcp\", \"--config\", \"/etc/mcp.json\"]\ndocs.web: URL https://docs.example/mcp"
    );
    let changed_declaring = docs_extension("https://changed.example/mcp", mcp);
    let changed_generation = generation(vec![extension("battery", &[]), changed_declaring]);
    let (changed_set, _) = super::mcp_grant_request(&changed_generation, &docs).unwrap();
    assert_ne!(set, changed_set);
}

#[test]
fn mcp_grant_detail_redacts_url_and_argv_credentials() {
    let mcp = ServiceSet::from_names(["mcp"]).unwrap();
    let skill = SkillRecord {
        name: name("docs"),
        description: "Documentation".into(),
        body: "Read documentation".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: [
                (
                    "api".into(),
                    McpServerDecl::Http {
                        url: "https://user:password@api.example:8443/mcp/v1?token=url-secret#fragment"
                            .into(),
                    },
                ),
                (
                    "cli".into(),
                    McpServerDecl::Stdio {
                        command: vec![
                            "mcp".into(),
                            "--token".into(),
                            "token-secret".into(),
                            "--token".into(),
                            "-leading-secret".into(),
                            "--token".into(),
                            "--api-key".into(),
                            "adjacent-key-secret".into(),
                            "-t".into(),
                            "short-secret".into(),
                            "--api-key=key-secret".into(),
                            "--authorization".into(),
                            "Bearer auth-secret".into(),
                            "-H".into(),
                            "Authorization: Bearer header-secret".into(),
                            "--header=Cookie: cookie-secret".into(),
                            "--verbose".into(),
                        ],
                        env: [
                            ("AUTH_TOKEN".into(), "env-secret".into()),
                            ("LD_PRELOAD".into(), "/tmp/lib.so".into()),
                        ]
                        .into(),
                    },
                ),
            ]
            .into(),
        }),
    };
    let declaring = ExtensionBuilder::new("docs", "1.0.0", mcp)
        .unwrap()
        .with_origin(Origin::User, None)
        .skill(skill)
        .build()
        .unwrap();
    let generation = generation(vec![declaring]);
    let (_, detail) = super::mcp_grant_request(&generation, &name("docs")).unwrap();
    assert_eq!(
        detail.as_ref(),
        "docs.api: URL https://api.example:8443/mcp/v1\ndocs.cli: command [\"mcp\", \"--token\", \"<redacted>\", \"--token\", \"<redacted>\", \"--token\", \"--api-key\", \"<redacted>\", \"-t\", \"<redacted>\", \"--api-key=<redacted>\", \"--authorization\", \"<redacted>\", \"-H\", \"<redacted>\", \"--header=<redacted>\", \"--verbose\"] env keys [AUTH_TOKEN, LD_PRELOAD]"
    );
    assert!(!detail.contains("password"));
    assert!(!detail.contains("url-secret"));
    assert!(!detail.contains("token-secret"));
    assert!(!detail.contains("-leading-secret"));
    assert!(!detail.contains("auth-secret"));
    assert!(!detail.contains("header-secret"));
    assert!(!detail.contains("cookie-secret"));
    assert!(!detail.contains("env-secret"));
    assert!(!detail.contains("/tmp/lib.so"));
}

#[test]
fn clear_drops_every_owner() {
    let generation = generation(vec![extension("battery", &[])]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(stub("mcp_web"), Visibility::Model)],
        )
        .unwrap();
    overlay.clear();
    assert!(
        overlay
            .publish(&generation, Arc::new(BTreeSet::new()))
            .entries()
            .is_empty()
    );
}
