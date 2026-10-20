//! Overlay staging, turn-boundary publication, promotion, and drop rules.

use std::collections::BTreeSet;
use std::sync::Arc;

use dal_core::ext::{McpBlock, McpEndpoint, McpLaunch, McpServerDecl};
use dal_core::{
    Caps, Mode, ModelId, ModelInfo, ModelRoute, Name, Origin, RawJson, ServiceSet, SkillRecord,
    ToolClass, ToolSpec, Visibility, Workspace,
};

use super::{Overlay, TurnTools};
use crate::error::ServiceError;
use crate::ext::generation::{Generation, ValidatedExtensions};
use crate::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use crate::ext::{BoxFuture, ExtensionBuilder};
use crate::session::context::{SearchSurface, ToolBoundaries, tool_list};

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
        provider: "".into(),
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
    let (specs, _) = tool_list(
        generation,
        tools,
        &model(),
        &model_id,
        Mode::Normal,
        SearchSurface::Client,
    );
    let mut names: Vec<String> = specs
        .into_iter()
        .map(|spec| spec.name.to_string())
        .collect();
    names.sort();
    names
}

fn deferred_names(generation: &Generation, tools: &TurnTools) -> Vec<String> {
    let model_id = ModelId::parse("acme/test").unwrap();
    let (_, deferred) = tool_list(
        generation,
        tools,
        &model(),
        &model_id,
        Mode::Normal,
        SearchSurface::Client,
    );
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
    let (specs, _) = tool_list(
        &generation,
        &next,
        &model(),
        &model_id,
        Mode::Normal,
        SearchSurface::Client,
    );
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
    assert_eq!(deferred_names(&generation, &after), [] as [String; 0]);
    assert_eq!(
        listed(&generation, &next),
        ["native", "tool_search"],
        "the earlier turn's snapshot never gains the promoted tool"
    );
}

fn tool_specs(
    generation: &Generation,
    tools: &TurnTools,
    surface: SearchSurface,
) -> Vec<dal_core::ModelToolSpec> {
    let model_id = ModelId::parse("acme/test").unwrap();
    tool_list(
        generation,
        tools,
        &model(),
        &model_id,
        Mode::Normal,
        surface,
    )
    .0
}

fn deferred_flags(specs: &[dal_core::ModelToolSpec]) -> Vec<(String, bool)> {
    let mut flags: Vec<(String, bool)> = specs
        .iter()
        .map(|spec| (spec.name.to_string(), spec.defer_loading))
        .collect();
    flags.sort();
    flags
}

#[test]
fn each_request_carries_exactly_one_search_surface() {
    let generation = generation(vec![
        extension("core", &["native"]),
        extension("battery", &[]),
    ]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![
                (stub("mcp_web"), Visibility::Deferred),
                (stub("mcp_docs"), Visibility::Deferred),
            ],
        )
        .unwrap();
    let turn = overlay.publish(&generation, Arc::new(BTreeSet::new()));

    let client = tool_specs(&generation, &turn, SearchSurface::Client);
    assert_eq!(
        deferred_flags(&client),
        [("native".into(), false), ("tool_search".into(), false)],
        "the client surface lists the core tool and no deferred spec"
    );

    let native = tool_specs(&generation, &turn, SearchSurface::Native);
    assert_eq!(
        deferred_flags(&native),
        [
            ("mcp_docs".into(), true),
            ("mcp_web".into(), true),
            ("native".into(), false)
        ],
        "the native surface defers every deferred tool, never the resident one, and lists no client tool_search"
    );
    assert_eq!(
        native.last().map(|spec| spec.name.as_ref()),
        Some("mcp_web"),
        "deferred tools follow the resident ones in byte order"
    );
    assert_eq!(
        deferred_names(&generation, &turn),
        ["mcp_docs", "mcp_web"],
        "the catalog the client search answers from is the same set"
    );
}

#[test]
fn no_deferred_tool_means_no_search_surface_on_either_family() {
    let generation = generation(vec![extension("core", &["native"])]);
    let turn = Overlay::default().publish(&generation, Arc::new(BTreeSet::new()));
    for surface in [SearchSurface::Client, SearchSurface::Native] {
        assert_eq!(
            deferred_flags(&tool_specs(&generation, &turn, surface)),
            [("native".into(), false)],
            "{surface:?}"
        );
    }
}

#[test]
fn a_registered_tool_search_owns_the_surface_so_nothing_is_deferred_natively() {
    let generation = generation(vec![
        extension("core", &["native", "tool_search"]),
        extension("battery", &[]),
    ]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(stub("mcp_web"), Visibility::Deferred)],
        )
        .unwrap();
    let turn = overlay.publish(&generation, Arc::new(BTreeSet::new()));
    assert_eq!(
        deferred_flags(&tool_specs(&generation, &turn, SearchSurface::Native)),
        [("native".into(), false), ("tool_search".into(), false)],
        "a second surface beside the registered one would be both, never one"
    );
}

#[test]
fn tool_boundaries_change_once_per_promotion_and_never_between() {
    let generation = generation(vec![
        extension("core", &["native"]),
        extension("battery", &[]),
    ]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![
                (stub("mcp_web"), Visibility::Deferred),
                (stub("mcp_docs"), Visibility::Deferred),
            ],
        )
        .unwrap();
    for surface in [SearchSurface::Client, SearchSurface::Native] {
        let mut boundaries = ToolBoundaries::default();
        let observe = |boundaries: &mut ToolBoundaries, promoted: &[&str]| {
            let promoted = Arc::new(promoted.iter().map(|tool| name(tool)).collect());
            let turn = overlay.publish(&generation, promoted);
            let tools: Arc<[dal_core::ModelToolSpec]> =
                tool_specs(&generation, &turn, surface).into();
            boundaries.observe(&tools)
        };
        for round in 0..10 {
            assert_eq!(
                observe(&mut boundaries, &[]),
                0,
                "{surface:?}: request {round} changed nothing"
            );
        }
        assert_eq!(
            observe(&mut boundaries, &["mcp_web", "mcp_docs"]),
            1,
            "{surface:?}: two tools promoted by one response are one boundary"
        );
        for round in 0..10 {
            assert_eq!(
                observe(&mut boundaries, &["mcp_web", "mcp_docs"]),
                1,
                "{surface:?}: request {round} after the promotion changed nothing"
            );
        }
    }
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

fn http_decl(url: &str) -> McpServerDecl {
    McpServerDecl::Http {
        endpoint: McpEndpoint::parse(url).expect("fixture endpoint"),
    }
}

fn opaque_command_decl(argv: &[&str], env: &[(&str, &str)]) -> McpServerDecl {
    McpServerDecl::Stdio {
        launch: McpLaunch::command(argv.iter().map(|arg| (*arg).into()).collect())
            .expect("fixture launch"),
        env: env
            .iter()
            .map(|(key, value)| (Box::<str>::from(*key), Box::<str>::from(*value)))
            .collect(),
    }
}

fn docs_skill(web_url: &str) -> SkillRecord {
    SkillRecord {
        name: name("docs"),
        description: "Documentation".into(),
        body: "Read documentation".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: [
                ("api".into(), http_decl("https://api.example/mcp")),
                (
                    "cli".into(),
                    opaque_command_decl(&["mcp", "--config", "/etc/mcp.json"], &[]),
                ),
                ("web".into(), http_decl(web_url)),
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
    let mcp = ServiceSet::EMPTY;
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
    let battery = name("battery");
    assert_eq!(tools.owner(&name("docs.web.list")), Some(&battery));
    assert_eq!(tools.owner(&name("docs.web.")), Some(&battery));
    assert_eq!(tools.owner(&name("docs.web.x.list")), Some(&battery));
    let declared_of = |tool: &str| {
        tools
            .entries()
            .iter()
            .find(|entry| entry.tool.name().as_str() == tool)
            .and_then(|entry| entry.mcp.as_ref())
    };
    let declared = declared_of("docs.web.list").expect("mapped tool preserves its declaration");
    assert_eq!(declared.plugin, docs);
    assert_eq!(declared.skill, name("docs"));
    assert!(declared.block.servers.contains_key("web"));
    assert!(declared_of("docs.web.").is_some_and(|entry| entry.block.servers.contains_key("web")));
    assert!(
        declared_of("docs.web.x.list").is_some_and(|entry| entry.block.servers.contains_key("web"))
    );
    let (set, detail) = super::mcp_grant_request(&base_generation, &docs).unwrap();
    let lines: Vec<&str> = detail.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "one grant line per declared server: {detail}"
    );
    assert_eq!(lines[0], "docs.api: https://api.example");
    assert!(
        lines[1].starts_with("docs.cli: "),
        "the command line names its server: {detail}"
    );
    assert_eq!(lines[2], "docs.web: https://docs.example");
    assert!(
        !detail.contains("mcp") && !detail.contains("--config"),
        "an opaque command reveals no program or option identity: {detail}"
    );
    assert!(
        !detail.contains("/etc/mcp.json"),
        "the grant detail leaks an operand: {detail}"
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
                // A validated public endpoint may still carry a secret in
                // its path or query; only the scheme and authority are public.
                (
                    "api".into(),
                    http_decl("https://api.example:8443/mcp/path-secret/v1?token=url-secret"),
                ),
                (
                    "cli".into(),
                    opaque_command_decl(
                        &[
                            "mcp",
                            "positional-secret",
                            "--token",
                            "token-secret",
                            "--token",
                            "-leading-secret",
                            "--client-secret",
                            "client-secret-value",
                            "--password",
                            "password-value",
                            "--access-token",
                            "access-token-value",
                            "--vendor-credential",
                            "custom-flag-secret",
                            "-t",
                            "short-secret",
                            "--api-key=key-secret",
                            "--header=Cookie: cookie-secret",
                            "Authorization: Bearer header-secret",
                            "--verbose",
                        ],
                        &[("AUTH_TOKEN", "env-secret"), ("LD_PRELOAD", "/tmp/lib.so")],
                    ),
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
        detail.lines().next(),
        Some("docs.api: https://api.example:8443"),
        "the endpoint reveals only scheme and authority: {detail}"
    );
    for leaked in [
        "/mcp/path-secret/v1",
        "url-secret",
        // An opaque command reveals no token at all: not its program, not
        // an option name, not any operand.
        "mcp",
        "positional-secret",
        "--token",
        "token-secret",
        "-leading-secret",
        "--client-secret",
        "client-secret-value",
        "--password",
        "password-value",
        "--access-token",
        "access-token-value",
        "--vendor-credential",
        "custom-flag-secret",
        "short-secret",
        "key-secret",
        "cookie-secret",
        "header-secret",
        // Environment names and values are both secret-capable.
        "AUTH_TOKEN",
        "env-secret",
        "LD_PRELOAD",
        "/tmp/lib.so",
    ] {
        assert!(
            !detail.contains(leaked),
            "the grant detail leaks {leaked}: {detail}"
        );
    }
}

fn notes_extension() -> crate::ext::Extension {
    ExtensionBuilder::new("notes", "1.0.0", ServiceSet::EMPTY)
        .unwrap()
        .with_origin(Origin::User, None)
        .skill(SkillRecord {
            name: name("memo"),
            description: "Notes".into(),
            body: "Take notes".into(),
            letter2image: false,
            mcp: None,
        })
        .build()
        .unwrap()
}

#[test]
fn an_allowlist_hides_and_blocks_every_tool_outside_it() {
    let generation = generation(vec![
        extension("core", &["native", "other_native"]),
        extension("battery", &[]),
    ]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![
                (stub("mcp_web"), Visibility::Model),
                (stub("mcp_docs"), Visibility::Model),
            ],
        )
        .unwrap();
    let turn = overlay.publish(&generation, Arc::new(BTreeSet::new()));
    let narrowed = turn.clone().restricted_to(Some(Arc::new(BTreeSet::from([
        name("native"),
        name("mcp_web"),
    ]))));
    assert_eq!(
        listed(&generation, &narrowed),
        ["mcp_web", "native"],
        "the model sees only allowed tools"
    );
    assert!(narrowed.tool(&generation, &name("native")).is_some());
    assert!(narrowed.tool(&generation, &name("mcp_web")).is_some());
    assert!(
        narrowed.tool(&generation, &name("other_native")).is_none(),
        "a generation tool outside the allowlist cannot be called"
    );
    assert!(
        narrowed.tool(&generation, &name("mcp_docs")).is_none(),
        "an overlay tool outside the allowlist cannot be called"
    );
    assert!(
        narrowed.owner(&name("mcp_docs")).is_none(),
        "a filtered overlay entry has no owner"
    );
    assert_eq!(narrowed.entries().len(), 1);

    let open = turn.restricted_to(None);
    assert_eq!(
        listed(&generation, &open),
        ["mcp_docs", "mcp_web", "native", "other_native"],
        "no allowlist leaves every tool visible"
    );
    assert!(open.tool(&generation, &name("other_native")).is_some());
}

#[test]
fn staging_rejects_each_bad_mcp_declaration_with_its_own_reason() {
    let generation = generation(vec![
        extension("battery", &[]),
        docs_extension("https://docs.example/mcp", ServiceSet::EMPTY),
        notes_extension(),
    ]);
    let overlay = Overlay::default();
    for (tool, declaring, reason) in [
        ("docs", "docs", "missing server segment"),
        ("docs.web", "docs", "missing remote tool segment"),
        ("docs..list", "docs", "server name must be non-empty"),
        ("dOcs.web.list", "docs", "skill name is invalid"),
        (
            "docs.web.list",
            "ghost",
            "declaring extension is not loaded",
        ),
        ("ghost.web.list", "docs", "declaring skill is not loaded"),
        ("memo.web.list", "notes", "skill has no MCP servers"),
        ("docs.ghost.list", "docs", "declared server is missing"),
    ] {
        let error = overlay
            .stage(
                &generation,
                &name("battery"),
                vec![(stub_declared(tool, Some(declaring)), Visibility::Deferred)],
            )
            .expect_err("a malformed or unloaded declaration is refused");
        let ServiceError::McpToolDeclaration {
            tool: rejected,
            plugin,
            reason: reported,
            ..
        } = error
        else {
            panic!("{tool}: expected McpToolDeclaration, got {error:?}");
        };
        assert_eq!(&*rejected, tool);
        assert_eq!(&*plugin, declaring, "{tool}");
        assert_eq!(reported, reason, "{tool}");
    }
    assert!(
        overlay
            .publish(&generation, Arc::new(BTreeSet::new()))
            .entries()
            .is_empty(),
        "every refused declaration staged nothing"
    );
    overlay
        .stage(
            &generation,
            &name("battery"),
            vec![(
                stub_declared("docs.web.list", Some("docs")),
                Visibility::Deferred,
            )],
        )
        .expect("the declared server stages");
}

#[test]
fn a_mapped_tool_retires_when_its_declaration_changes_on_reload() {
    let before = generation(vec![
        extension("battery", &[]),
        docs_extension("https://docs.example/mcp", ServiceSet::EMPTY),
    ]);
    let overlay = Overlay::default();
    overlay
        .stage(
            &before,
            &name("battery"),
            vec![
                (
                    stub_declared("docs.web.list", Some("docs")),
                    Visibility::Model,
                ),
                (stub("plain"), Visibility::Model),
            ],
        )
        .unwrap();
    let none = Arc::new(BTreeSet::new());
    assert_eq!(
        listed(&before, &overlay.publish(&before, Arc::clone(&none))),
        ["docs.web.list", "plain"]
    );
    let changed = generation(vec![
        extension("battery", &[]),
        docs_extension("https://changed.example/mcp", ServiceSet::EMPTY),
    ]);
    assert_eq!(
        listed(&changed, &overlay.publish(&changed, none)),
        ["plain"],
        "a changed MCP declaration retires its mapped tools and keeps the rest"
    );
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
