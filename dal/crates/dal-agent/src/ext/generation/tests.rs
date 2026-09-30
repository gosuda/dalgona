//! Generation validation, ordering, and snapshot tests.
//!
//! Built only from [`ExtensionBuilder`] and the generation API: no broker,
//! no host, no filesystem. Deterministic: canonical order and conflict
//! outcomes never depend on input order beyond the documented rules.

use std::sync::Arc;

use dal_core::ext::{McpBlock, McpServerDecl};
use dal_core::{
    Caps, Claimant, Mode, ModelId, ModelInfo, ModelRoute, Name, Origin, RawJson, RegistrationError,
    ServiceSet, SkillRecord, ToolCallEvent, ToolCallVerdict, ToolClass, ToolSpec, Visibility,
    Workspace,
};

use super::super::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use super::super::{BoxFuture, ExtensionBuilder, Hook, HookCx, HookError};
use super::{Generation, ValidatedExtensions};

/// One test tool with a fixed spec; every instance stays live through the
/// `Arc` that holds its generation.
struct StubTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for StubTool {
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

/// One `tool_call` hook registering order without effects.
struct AllowHook;

impl Hook<ToolCallEvent, ToolCallVerdict> for AllowHook {
    fn call(
        &self,
        _input: ToolCallEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        Box::pin(async { Ok(ToolCallVerdict::Allow) })
    }
}

fn ext(name: &str, origin: Origin) -> ExtensionBuilder {
    ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
        .expect("valid extension identity")
        .with_origin(origin, None)
}

fn stub_tool(name: &str) -> Arc<StubTool> {
    let tool_name: Name = name.parse().expect("valid tool name");
    Arc::new(StubTool {
        name: tool_name.clone(),
        spec: Arc::new(ToolSpec {
            name: tool_name,
            description: format!("test tool {name}").into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("valid schema"),
            grammar: None,
        }),
    })
}

fn model_info() -> ModelInfo {
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

fn model_id() -> ModelId {
    ModelId::parse("acme/test").expect("valid model id")
}

#[test]
fn generation_order_uses_origin_and_plugin_name() {
    let builtin_second = ext("core-b", Origin::Builtin)
        .tool(stub_tool("t_core_b"), Visibility::Model)
        .on_tool_call(AllowHook)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid builtin extension");
    let bundled_zebra = ext("zebra", Origin::Bundled)
        .tool(stub_tool("t_zebra"), Visibility::Model)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid bundled extension");
    let user_delta = ext("delta", Origin::User)
        .tool(stub_tool("t_delta"), Visibility::Model)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid user extension");
    let user_charlie = ext("charlie", Origin::User)
        .tool(stub_tool("t_charlie"), Visibility::Model)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid user extension");
    let bundled_apple = ext("apple", Origin::Bundled)
        .tool(stub_tool("t_apple"), Visibility::Model)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid bundled extension");
    let builtin_first = ext("core-a", Origin::Builtin)
        .tool(stub_tool("t_core_a"), Visibility::Model)
        .on_tool_call(AllowHook)
        .build()
        .expect("valid builtin extension");
    let validated = ValidatedExtensions::validate(
        vec![
            user_delta,
            bundled_zebra,
            builtin_second,
            user_charlie,
            bundled_apple,
            builtin_first,
        ],
        None,
    )
    .expect("distinct records validate");
    let generation = Generation::build(validated);
    let order: Vec<&str> = generation
        .extensions
        .iter()
        .map(super::super::Extension::name)
        .collect();
    assert_eq!(
        order,
        vec!["core-b", "core-a", "apple", "zebra", "charlie", "delta"],
        "builtins keep input order, bundled and user sort by name"
    );
}

#[test]
fn skill_mcp_requires_declaring_extension_to_inject_mcp() {
    let skill = SkillRecord {
        name: "docs".parse().expect("skill name"),
        description: "Documentation tools".into(),
        body: "Use docs tools".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: [(
                "web".into(),
                McpServerDecl::Http {
                    url: "https://docs.example/mcp".into(),
                },
            )]
            .into(),
        }),
    };
    let extension = ExtensionBuilder::new("docs", "1.0.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .skill(skill.clone())
        .build()
        .expect("builder seals values before whole-generation validation");
    let error = ValidatedExtensions::validate(vec![extension], None)
        .expect_err("a declared MCP skill needs the mcp inject")
        .to_string();
    assert_eq!(
        error,
        "skill \"docs\" in extension \"docs\" declares MCP servers but does not inject \"mcp\""
    );

    let extension = ExtensionBuilder::new(
        "docs",
        "1.0.0",
        ServiceSet::from_names(["mcp"]).expect("known service"),
    )
    .expect("extension builder")
    .skill(skill)
    .build()
    .expect("valid extension");
    assert!(
        ValidatedExtensions::validate(vec![extension], None).is_ok(),
        "declaring extension injects mcp"
    );
}

#[test]
fn generation_rejects_conflicts_before_publish() {
    let product = ext("core", Origin::Builtin)
        .tool(stub_tool("dup"), Visibility::Model)
        .build()
        .expect("valid product extension");
    let product_gen = Generation::build(
        ValidatedExtensions::validate(vec![product], None).expect("product validates"),
    );
    let mut published: Option<Arc<Generation>> = None;
    let plugin = ext("focus", Origin::User)
        .tool(stub_tool("dup"), Visibility::Model)
        .build()
        .expect("valid plugin extension");
    match ValidatedExtensions::validate(vec![plugin], Some(&product_gen)) {
        Ok(validated) => {
            published = Some(Arc::new(Generation::build(validated)));
        }
        Err(err) => {
            let text = err.to_string();
            assert!(text.contains("\"dup\""), "error names the record: {text}");
            assert!(
                text.contains("the built-in extension core"),
                "error names the first claimant: {text}"
            );
            let RegistrationError::Conflict {
                kind,
                name,
                claimant,
            } = err
            else {
                panic!("expected a conflict error, got {text}");
            };
            let expected: Name = "dup".parse().expect("valid tool name");
            let owner: Name = "core".parse().expect("valid extension name");
            assert_eq!(kind, "tool");
            assert_eq!(name, expected);
            assert_eq!(claimant, Claimant::Builtin(owner));
            assert_ne!(
                claimant,
                Claimant::Plugin("focus".parse().expect("valid extension name"))
            );
        }
    }
    assert!(published.is_none(), "failed validation publishes nothing");
    let first = ext("aaa", Origin::User)
        .tool(stub_tool("dup"), Visibility::Model)
        .build()
        .expect("valid extension");
    let second = ext("bbb", Origin::User)
        .tool(stub_tool("dup"), Visibility::Model)
        .build()
        .expect("valid extension");
    let err =
        ValidatedExtensions::validate(vec![second, first], None).expect_err("duplicate must fail");
    let text = err.to_string();
    assert!(
        text.contains("plugin \"aaa\""),
        "error names the first claimant: {text}"
    );
    let RegistrationError::Conflict {
        kind,
        name,
        claimant,
    } = err
    else {
        panic!("expected a conflict error, got {text}");
    };
    let expected: Name = "dup".parse().expect("valid tool name");
    let first_owner: Name = "aaa".parse().expect("valid extension name");
    assert_eq!(kind, "tool");
    assert_eq!(name, expected);
    assert_eq!(claimant, Claimant::Plugin(first_owner));
    assert_ne!(
        claimant,
        Claimant::Plugin("bbb".parse().expect("valid extension name"))
    );
}

#[test]
fn generation_keeps_model_specs_and_old_handlers_stable() {
    let keep: Name = "keep".parse().expect("valid tool name");
    let model = model_info();
    let model_id = model_id();
    let old_tool = stub_tool("keep");
    let old_gen = Arc::new(Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("core", Origin::Builtin)
                    .tool(old_tool, Visibility::Model)
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("old generation validates"),
    ));
    let old_spec = old_gen
        .tool_spec(&keep, &model, &model_id)
        .expect("old tool resolves");
    let new_gen = Arc::new(Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("core", Origin::Builtin)
                    .tool(stub_tool("keep"), Visibility::Model)
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("replacement validates"),
    ));
    let old_again = old_gen
        .tool_spec(&keep, &model, &model_id)
        .expect("old snapshot still resolves");
    assert!(
        Arc::ptr_eq(&old_spec, &old_again),
        "the spec cache is stable within one generation"
    );
    let (handler, visibility) = old_gen.tool(&keep).expect("old tool stays live");
    assert_eq!(visibility, Visibility::Model);
    assert_eq!(handler.name().as_str(), "keep");
    assert_eq!(handler.spec(&model).description, "test tool keep".into());
    let new_spec = new_gen
        .tool_spec(&keep, &model, &model_id)
        .expect("replacement resolves");
    assert!(
        !Arc::ptr_eq(&old_spec, &new_spec),
        "the replacement carries its own specs"
    );
    assert_ne!(
        old_gen.id.get(),
        new_gen.id.get(),
        "every build mints a fresh id"
    );
    assert!(
        new_gen.tool(&keep).is_some(),
        "the next snapshot sees the new generation"
    );
}

#[test]
fn promotion_rebuilds_before_next_infer() {
    let deep: Name = "deep_search".parse().expect("valid tool name");
    let before = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("finder", Origin::User)
                    .tool(stub_tool("deep_search"), Visibility::Deferred)
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("deferred tool validates"),
    );
    assert_eq!(before.tool_visibility(&deep), Some(Visibility::Deferred));
    let (before_tools, deferred) = crate::session::context::tool_list(
        &before,
        &crate::ext::overlay::TurnTools::empty(),
        &model_info(),
        &ModelId::parse("acme/test").unwrap(),
        Mode::Normal,
    );
    assert_eq!(
        before_tools.last().map(|tool| tool.name.as_ref()),
        Some("tool_search")
    );
    assert_eq!(deferred.len(), 1);
    let after = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("finder", Origin::User)
                    .tool(stub_tool("deep_search"), Visibility::Model)
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("promoted tool validates"),
    );
    assert_eq!(after.tool_visibility(&deep), Some(Visibility::Model));
    let plain = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("core", Origin::Builtin)
                    .tool(stub_tool("read"), Visibility::Model)
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("model-only tools validate"),
    );
    let search: Name = "tool_search".parse().expect("valid tool name");
    assert!(
        plain.tool(&search).is_none(),
        "no tool_search registration without deferred tools"
    );
    let (plain_tools, plain_deferred) = crate::session::context::tool_list(
        &plain,
        &crate::ext::overlay::TurnTools::empty(),
        &model_info(),
        &ModelId::parse("acme/test").unwrap(),
        Mode::Normal,
    );
    assert!(
        plain_tools
            .iter()
            .all(|tool| tool.name.as_ref() != "tool_search"),
        "no search tool without deferred tools"
    );
    assert!(plain_deferred.is_empty());
}

#[test]
fn reload_splice_replaces_only_the_plugin_tail() {
    use super::splice_plugins;

    let current = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("a", Origin::Builtin).build().expect("product a"),
                ext("b", Origin::Builtin).build().expect("product b"),
                ext("battery", Origin::Bundled)
                    .build()
                    .expect("product battery"),
                ext("p1", Origin::User).build().expect("plugin p1"),
            ],
            None,
        )
        .expect("current validates"),
    );
    let next = Generation::build(
        ValidatedExtensions::validate(
            splice_plugins(
                &current.extensions,
                vec![ext("p2", Origin::User).build().expect("plugin p2")],
            ),
            None,
        )
        .expect("replacement validates"),
    );
    let order: Vec<&str> = next
        .extensions
        .iter()
        .map(super::super::Extension::name)
        .collect();
    assert_eq!(order, vec!["a", "b", "battery", "p2"]);
}

#[test]
fn generation_rejects_duplicate_doc_uris() {
    let only = ext("manual", Origin::User)
        .doc("index", "Index", "first")
        .doc("index", "Index", "second")
        .build()
        .expect("valid extension");
    let err = ValidatedExtensions::validate(vec![only], None).expect_err("duplicate uri");
    let text = err.to_string();
    assert_eq!(text, "doc uri 'manual://index' is already registered");
    assert!(matches!(err, RegistrationError::DuplicateDocUri { .. }));
}

#[test]
fn generation_rejects_second_doc_scheme_with_same_name() {
    let first = ext("wiki", Origin::User)
        .doc("a", "A", "first")
        .build()
        .expect("valid extension");
    let second = ext("wiki", Origin::User)
        .doc("b", "B", "second")
        .build()
        .expect("valid extension");
    let err =
        ValidatedExtensions::validate(vec![first, second], None).expect_err("scheme conflict");
    assert!(matches!(
        err,
        RegistrationError::Conflict { kind: "scheme", .. }
    ));
}

#[test]
fn generation_publishes_doc_pages_sorted_with_lookup() {
    let zeta = ext("zeta", Origin::User)
        .doc("b", "B", "bee")
        .doc("a", "A", "ay")
        .build()
        .expect("valid extension");
    let alpha = ext("alpha", Origin::User)
        .doc("m", "M", "em")
        .build()
        .expect("valid extension");
    let generation = Generation::build(
        ValidatedExtensions::validate(vec![zeta, alpha], None).expect("docs validate"),
    );
    let page = generation.docs().find("alpha://m").expect("lookup hits");
    assert_eq!(page.title.as_ref(), "M");
    assert_eq!(page.text.as_ref(), "em");
    assert!(generation.docs().find("alpha://missing").is_none());
    let uris: Vec<&str> = generation
        .docs()
        .list()
        .iter()
        .map(|page| page.uri.as_ref())
        .collect();
    assert_eq!(uris, ["alpha://m", "zeta://a", "zeta://b"]);
}

#[test]
fn generation_bars_user_manual_schemes_but_keeps_builtin_manuals() {
    for scheme in ["dal", "dalgona"] {
        let user = ext(scheme, Origin::User)
            .doc("index", "Index", "shadow")
            .build()
            .expect("valid extension");
        let err = ValidatedExtensions::validate(vec![user], None).expect_err("user manual scheme");
        assert!(matches!(
            err,
            RegistrationError::Conflict { kind: "scheme", .. }
        ));
        let builtin = ext(scheme, Origin::Builtin)
            .doc("index", "Index", "manual")
            .build()
            .expect("valid extension");
        let generation = Generation::build(
            ValidatedExtensions::validate(vec![builtin], None).expect("builtin manual validates"),
        );
        assert!(
            generation
                .docs()
                .find(&format!("{scheme}://index"))
                .is_some()
        );
    }
}
