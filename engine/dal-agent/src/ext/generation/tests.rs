//! Generation validation, ordering, and snapshot tests.
//!
//! Built only from [`ExtensionBuilder`] and the generation API: no broker,
//! no host, no filesystem. Deterministic: canonical order and conflict
//! outcomes never depend on input order beyond the documented rules.

use std::sync::Arc;

use dal_core::ext::{McpBlock, McpEndpoint, McpServerDecl};
use dal_core::{
    Caps, Claimant, CommandName, CommandSpec, Mode, ModelId, ModelInfo, ModelRequest, ModelRoute,
    Name, Origin, RawJson, RegistrationError, Reply, RuleRecord, ServiceSet, SkillRecord,
    ToolCallEvent, ToolCallVerdict, ToolClass, ToolSpec, Visibility, Workspace,
};
use dal_provider::EventStream;

use super::super::prompt::{PromptOrder, PromptSection};
use super::super::scheme::{Doc, SchemeCx, SchemeResolver};
use super::super::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome};
use super::super::{
    BoxFuture, CommandCx, CommandHandler, CompactError, CompactInput, Compaction, Compactor,
    ExtensionBuilder, Hook, HookCx, HookError, ModelCx, ModelError, ModelHandler, ModelRecord,
    Services, StatusCx, StatusPoll, StatusSnapshot,
};
use super::{Generation, ValidatedExtensions};
use crate::error::{SchemeError, ServiceError};

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

/// One scheme resolver whose reads never run; only its claim matters.
struct StubScheme;

impl SchemeResolver for StubScheme {
    fn read<'a>(
        &'a self,
        _path: &'a str,
        _cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async {
            Err(SchemeError::NotFound {
                uri: "stub://none".into(),
            })
        })
    }
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
fn skill_mcp_declaration_does_not_require_the_mcp_inject() {
    let skill = SkillRecord {
        name: "docs".parse().expect("skill name"),
        description: "Documentation tools".into(),
        body: "Use docs tools".into(),
        letter2image: false,
        mcp: Some(McpBlock {
            servers: [(
                "web".into(),
                McpServerDecl::Http {
                    endpoint: McpEndpoint::parse("https://docs.example/mcp")
                        .expect("fixture endpoint"),
                },
            )]
            .into(),
        }),
    };
    let extension = ExtensionBuilder::new("docs", "1.0.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .skill(skill)
        .build()
        .expect("builder seals values before whole-generation validation");
    let validated = ValidatedExtensions::validate(vec![extension], None)
        .expect("a declared MCP skill needs no mcp inject");
    assert!(
        validated
            .0
            .iter()
            .any(|extension| extension.name() == "docs"),
        "declaring extension loads without the mcp inject"
    );
}

#[test]
fn the_child_policy_extension_name_is_builtin_only() {
    for origin in [Origin::Bundled, Origin::User] {
        let plugin = ext("dal-agent", origin).build().expect("valid identity");
        let err = ValidatedExtensions::validate(vec![plugin], None)
            .expect_err("a non-builtin dal-agent extension is rejected");
        assert!(matches!(
            err,
            RegistrationError::Conflict {
                kind: "extension",
                ..
            }
        ));
    }
    let builtin = ext("dal-agent", Origin::Builtin)
        .build()
        .expect("valid identity");
    assert!(ValidatedExtensions::validate(vec![builtin], None).is_ok());
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
                rival,
            } = err
            else {
                panic!("expected a conflict error, got {text}");
            };
            let expected: Name = "dup".parse().expect("valid tool name");
            let owner: Name = "core".parse().expect("valid extension name");
            let second: Name = "focus".parse().expect("valid extension name");
            assert_eq!(kind, "tool");
            assert_eq!(name, expected);
            assert_eq!(claimant, Claimant::Builtin(owner));
            assert_eq!(rival, Claimant::Plugin(second));
            assert!(
                text.contains("plugin \"focus\""),
                "error names the rival: {text}"
            );
            assert!(
                text.contains("cannot register it"),
                "error names both claimants: {text}"
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
        rival,
    } = err
    else {
        panic!("expected a conflict error, got {text}");
    };
    let expected: Name = "dup".parse().expect("valid tool name");
    let first_owner: Name = "aaa".parse().expect("valid extension name");
    let second_owner: Name = "bbb".parse().expect("valid extension name");
    assert_eq!(kind, "tool");
    assert_eq!(name, expected);
    assert_eq!(claimant, Claimant::Plugin(first_owner));
    assert_eq!(rival, Claimant::Plugin(second_owner));
    assert!(
        text.contains("plugin \"bbb\""),
        "error names the rival: {text}"
    );
    assert!(
        text.contains("cannot register it"),
        "error names both claimants: {text}"
    );
}

/// One slash command stub for duplicate-command tests.
struct StubCommand;

impl CommandHandler for StubCommand {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        _cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async { Ok(Reply::Queued { turn: None }) })
    }
}

#[test]
fn generation_names_both_claimants_for_duplicate_commands() {
    let command = |owner: &str| {
        ext(owner, Origin::User)
            .command(
                CommandSpec {
                    name: CommandName::parse("review").expect("valid command name"),
                    summary: "Test review.".into(),
                    args_hint: None,
                },
                Arc::new(StubCommand),
            )
            .build()
            .expect("valid extension")
    };
    let first = command("aaa");
    let second = command("bbb");
    let err =
        ValidatedExtensions::validate(vec![second, first], None).expect_err("duplicate must fail");
    assert_eq!(
        err.to_string(),
        "command \"review\" is already registered by plugin \"aaa\"; plugin \"bbb\" cannot register it"
    );
    let RegistrationError::Conflict {
        kind,
        name,
        claimant,
        rival,
    } = err
    else {
        panic!("expected a conflict error");
    };
    assert_eq!(kind, "command");
    assert_eq!(name.as_str(), "review");
    assert_eq!(
        claimant,
        Claimant::Plugin("aaa".parse().expect("valid extension name"))
    );
    assert_eq!(
        rival,
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
        crate::session::context::SearchSurface::Client,
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
        crate::session::context::SearchSurface::Client,
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
                Vec::new(),
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
fn reload_splice_keeps_rebuilt_builtins_at_their_startup_position() {
    use super::splice_plugins;

    // `/reload` rebuilds a builtin in place (the skills/letter pair): the
    // replacement must land at its startup index or the canonical order
    // shifts and the published non-`User` names diverge.
    let current = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("a", Origin::Builtin).build().expect("product a"),
                ext("skills", Origin::Builtin)
                    .build()
                    .expect("product skills"),
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
    let base: Vec<&str> = current
        .extensions
        .iter()
        .filter(|ext| ext.origin() != Origin::User)
        .map(super::super::Extension::name)
        .collect();
    let next = Generation::build(
        ValidatedExtensions::validate(
            splice_plugins(
                &current.extensions,
                vec![ext("p2", Origin::User).build().expect("plugin p2")],
                vec![
                    ext("skills", Origin::Builtin)
                        .build()
                        .expect("rebuilt skills"),
                ],
            ),
            None,
        )
        .expect("replacement validates"),
    );
    let next_base: Vec<&str> = next
        .extensions
        .iter()
        .filter(|ext| ext.origin() != Origin::User)
        .map(super::super::Extension::name)
        .collect();
    assert_eq!(base, next_base, "non-plugin names keep startup order");
    let order: Vec<&str> = next
        .extensions
        .iter()
        .map(super::super::Extension::name)
        .collect();
    assert_eq!(order, vec!["a", "skills", "b", "battery", "p2"]);
}

/// A model handler that never runs; only the registration matters.
struct NoModel;

impl ModelHandler for NoModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { Err(ModelError::PrivateRounds) })
    }
}

/// A status poll that reports quiet; only its kind name matters.
struct QuietStatus;

impl StatusPoll for QuietStatus {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: true,
            text: None,
        }
    }
}

fn skill_record(name: &str) -> SkillRecord {
    SkillRecord {
        name: name.parse().expect("valid skill name"),
        description: "test skill".into(),
        body: "body".into(),
        letter2image: false,
        mcp: None,
    }
}

fn rule_record(name: &str) -> RuleRecord {
    RuleRecord {
        name: name.parse().expect("valid rule name"),
        patterns: vec!["needle".into()],
        text: "reminder".into(),
        judge: None,
        scope: None,
        globs: None,
        agents: None,
        mode: None,
        repeat_mode: None,
        repeat_gap: None,
        always_apply: false,
        report: false,
        enabled: true,
    }
}

fn model_record(id: &str) -> ModelRecord {
    ModelRecord {
        id: ModelId::parse(id).expect("valid model id"),
        caps: model_info().caps,
        handler: Arc::new(NoModel),
        export: None,
    }
}

type Register = fn(ExtensionBuilder) -> ExtensionBuilder;

#[test]
fn every_record_kind_claims_its_name_across_extensions() {
    let kinds: [(&str, Register); 5] = [
        ("skill", |builder| builder.skill(skill_record("shared"))),
        ("rule", |builder| builder.rule(rule_record("shared"))),
        ("model", |builder| {
            builder.model(model_record("acme/shared"))
        }),
        ("compactor", |builder| {
            builder.compactor("shared", Arc::new(DecliningCompactor))
        }),
        ("status", |builder| {
            builder.status_kind("shared", Arc::new(QuietStatus))
        }),
    ];
    for (kind, register) in kinds {
        let first = register(ext("aaa", Origin::User))
            .build()
            .expect("valid extension");
        let second = register(ext("bbb", Origin::User))
            .build()
            .expect("valid extension");
        let err = ValidatedExtensions::validate(vec![second, first], None)
            .expect_err("the second claimant of one name conflicts");
        let RegistrationError::Conflict {
            kind: reported,
            claimant,
            rival,
            ..
        } = err
        else {
            panic!("{kind}: expected a conflict, got {err:?}");
        };
        assert_eq!(reported, kind);
        assert_eq!(
            claimant,
            Claimant::Plugin("aaa".parse().expect("valid extension name")),
            "{kind}: canonical order makes the earlier name the claimant"
        );
        assert_eq!(
            rival,
            Claimant::Plugin("bbb".parse().expect("valid extension name")),
            "{kind}"
        );
    }
}

#[test]
fn one_name_under_different_record_kinds_never_conflicts() {
    let tool_owner = ext("aaa", Origin::User)
        .tool(stub_tool("shared"), Visibility::Model)
        .build()
        .expect("valid extension");
    let other_kinds = ext("bbb", Origin::User)
        .skill(skill_record("shared"))
        .rule(rule_record("shared"))
        .compactor("shared", Arc::new(DecliningCompactor))
        .status_kind("shared", Arc::new(QuietStatus))
        .command(
            CommandSpec {
                name: CommandName::parse("shared").expect("valid command name"),
                summary: "Test command.".into(),
                args_hint: None,
            },
            Arc::new(StubCommand),
        )
        .build()
        .expect("valid extension");
    ValidatedExtensions::validate(vec![tool_owner, other_kinds], None)
        .expect("record kinds claim names in separate ledgers");
}

#[test]
fn a_batch_record_shadowing_the_previous_generation_names_the_previous_owner() {
    let previous = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("core", Origin::Builtin)
                    .skill(skill_record("shared"))
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("the previous generation validates"),
    );
    let rival = ext("focus", Origin::User)
        .skill(skill_record("shared"))
        .build()
        .expect("valid extension");
    let err = ValidatedExtensions::validate(vec![rival], Some(&previous))
        .expect_err("a skill already live in the previous generation conflicts");
    let RegistrationError::Conflict {
        kind,
        claimant,
        rival,
        ..
    } = err
    else {
        panic!("expected a conflict, got {err:?}");
    };
    assert_eq!(kind, "skill");
    assert_eq!(
        claimant,
        Claimant::Builtin("core".parse().expect("valid extension name"))
    );
    assert_eq!(
        rival,
        Claimant::Plugin("focus".parse().expect("valid extension name"))
    );
}

/// A mapped MCP tool: its name encodes `<skill>.<server>.<tool>` and it
/// names the extension whose skill declares the server.
struct MappedTool {
    name: Name,
    declaring: Name,
    spec: Arc<ToolSpec>,
}

impl Tool for MappedTool {
    fn declaring_extension(&self) -> Option<Name> {
        Some(self.declaring.clone())
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

fn mapped_tool(name: &str, declaring: &str) -> Arc<MappedTool> {
    let name = Name::parse_mapped_tool(name).expect("mapped tool name");
    Arc::new(MappedTool {
        spec: Arc::new(ToolSpec {
            name: name.clone(),
            description: "mapped test tool".into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("valid schema"),
            grammar: None,
        }),
        name,
        declaring: declaring.parse().expect("declaring extension name"),
    })
}

#[test]
fn mapped_mcp_tools_are_rejected_with_the_reason_each_declaration_fails() {
    let docs = ext("docs", Origin::User)
        .skill(SkillRecord {
            name: "docs".parse().expect("skill name"),
            description: "Documentation tools".into(),
            body: "Use docs tools".into(),
            letter2image: false,
            mcp: Some(McpBlock {
                servers: [(
                    "web".into(),
                    McpServerDecl::Http {
                        endpoint: McpEndpoint::parse("https://docs.example/mcp")
                            .expect("fixture endpoint"),
                    },
                )]
                .into(),
            }),
        })
        .build()
        .expect("valid extension");
    let notes = ext("notes", Origin::User)
        .skill(skill_record("memo"))
        .build()
        .expect("valid extension");
    let shape = "mapped tool name must be <skill>.<server>.<tool>";
    let empty = "server and remote tool names must be non-empty";
    for (tool, declaring, reason) in [
        ("docs", "docs", shape),
        ("docs.web", "docs", shape),
        ("docs..list", "docs", empty),
        ("docs.web.", "docs", empty),
        ("dOcs.web.list", "docs", "mapped tool skill name is invalid"),
        (
            "docs.web.list",
            "ghost",
            "declaring extension is not loaded",
        ),
        ("ghost.web.list", "docs", "declaring skill is not loaded"),
        (
            "memo.web.list",
            "notes",
            "declaring skill has no MCP servers",
        ),
        ("docs.ghost.list", "docs", "declared server is missing"),
    ] {
        let battery = ext("battery", Origin::User)
            .tool(mapped_tool(tool, declaring), Visibility::Model)
            .build()
            .expect("valid extension");
        let err = ValidatedExtensions::validate(vec![battery, docs.clone(), notes.clone()], None)
            .expect_err("a bad declaration is refused before publish");
        let RegistrationError::InvalidMcpTool {
            tool: rejected,
            extension,
            reason: reported,
        } = err
        else {
            panic!("{tool}: expected InvalidMcpTool, got {err:?}");
        };
        assert_eq!(&*rejected, tool);
        assert_eq!(extension.as_str(), declaring, "{tool}");
        assert_eq!(reported, reason, "{tool}");
    }
    let battery = ext("battery", Origin::User)
        .tool(mapped_tool("docs.web.list", "docs"), Visibility::Model)
        .build()
        .expect("valid extension");
    ValidatedExtensions::validate(vec![battery, docs, notes], None)
        .expect("a declared server validates");
}

/// A tool whose schema text names the route it was asked for.
struct RouteTool {
    name: Name,
}

impl Tool for RouteTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::new(ToolSpec {
            name: self.name.clone(),
            description: format!("{:?}", model.route).into(),
            parameters: RawJson::parse(r#"{"type":"object"}"#).expect("valid schema"),
            grammar: None,
        })
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Interrupted })
    }
}

#[test]
fn the_spec_cache_never_shares_a_schema_between_routes_under_one_model_id() {
    let tool_name: Name = "route_aware".parse().expect("valid tool name");
    let generation = Generation::build(
        ValidatedExtensions::validate(
            vec![
                ext("core", Origin::Builtin)
                    .tool(
                        Arc::new(RouteTool {
                            name: tool_name.clone(),
                        }),
                        Visibility::Model,
                    )
                    .build()
                    .expect("valid extension"),
            ],
            None,
        )
        .expect("validates"),
    );
    let model_id = model_id();
    let on_route = |id: &str| ModelInfo {
        route: ModelRoute::from_id(id),
        ..model_info()
    };
    let first = generation
        .tool_spec(&tool_name, &on_route("acme/a"), &model_id)
        .expect("route a resolves");
    let again = generation
        .tool_spec(&tool_name, &on_route("acme/a"), &model_id)
        .expect("route a resolves again");
    assert!(
        Arc::ptr_eq(&first, &again),
        "one route under one id hits the cache"
    );
    let other = generation
        .tool_spec(&tool_name, &on_route("acme/b"), &model_id)
        .expect("route b resolves");
    assert_ne!(
        first.description, other.description,
        "a second route under the same model id gets its own schema"
    );
    assert!(!Arc::ptr_eq(&first, &other));
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
    // Same-named extensions already fail the extension-name claim, so
    // this scenario claims the scheme name directly: "alpha" registers
    // `wiki://` before the "wiki" extension's docs scheme is checked.
    let first = ext("alpha", Origin::User)
        .scheme("wiki", Arc::new(StubScheme))
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

/// A compactor that always declines; only its place in the chain matters.
struct DecliningCompactor;

impl Compactor for DecliningCompactor {
    fn compact<'a>(
        &'a self,
        _input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

#[test]
fn compaction_chain_runs_every_primary_before_any_fallback() {
    let builtin = ext("core", Origin::Builtin)
        .compactor("remote", Arc::new(DecliningCompactor))
        .fallback_compactor("summary", Arc::new(DecliningCompactor))
        .build()
        .expect("valid builtin extension");
    let bundled = ext("battery", Origin::Bundled)
        .compactor("images", Arc::new(DecliningCompactor))
        .fallback_compactor("last-bundled", Arc::new(DecliningCompactor))
        .build()
        .expect("valid bundled extension");
    let user = ext("plugin", Origin::User)
        .compactor("custom", Arc::new(DecliningCompactor))
        .build()
        .expect("valid user extension");
    assert!(builtin.is_fallback_compactor("summary"));
    assert!(!builtin.is_fallback_compactor("remote"));

    let generation = Generation::build(
        ValidatedExtensions::validate(vec![user, bundled, builtin], None)
            .expect("extensions validate"),
    );

    let chain: Vec<&str> = generation
        .compactors
        .entries()
        .iter()
        .map(|entry| entry.name.as_ref())
        .collect();
    assert_eq!(
        chain,
        ["remote", "images", "custom", "summary", "last-bundled"],
        "primaries keep origin order, then fallbacks keep origin order"
    );
    for name in &chain {
        assert!(
            generation.compactor(name).is_some(),
            "{name} resolves to its compactor after regrouping"
        );
    }
}
#[test]
fn load_order_independence() {
    // Filesystem iteration order never leaks into the settled generation:
    // User extensions settle by name, so reversed inputs build the same
    // extension order, prompt-section order, and tool bytes. A sort that
    // kept input order would put zzz first for the reversed input and
    // fail the equality pins.
    let named = |name: &str| {
        ext(name, Origin::User)
            .tool(stub_tool(&format!("{name}_tool")), Visibility::Model)
            .prompt_section(PromptSection::static_text(
                PromptOrder::Identity,
                format!("id-{name}").into(),
            ))
            .build()
            .expect("valid extension")
    };
    let first = Generation::build(
        ValidatedExtensions::validate(vec![named("aaa"), named("zzz")], None).expect("validates"),
    );
    let second = Generation::build(
        ValidatedExtensions::validate(vec![named("zzz"), named("aaa")], None).expect("validates"),
    );
    let extension_order = |generation: &Generation| {
        generation
            .extensions
            .iter()
            .map(|extension| extension.name().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        extension_order(&first),
        vec!["aaa".to_owned(), "zzz".to_owned()],
        "user extensions settle by name"
    );
    assert_eq!(
        extension_order(&second),
        extension_order(&first),
        "reversed input builds the same extension order"
    );
    let section_orders = |generation: &Generation| {
        generation
            .prompt_sections
            .iter()
            .map(|(_, section)| section.order())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        section_orders(&second),
        section_orders(&first),
        "reversed input builds the same prompt-section order"
    );
    let tool_order = |generation: &Generation| {
        generation
            .tools
            .entries()
            .iter()
            .map(|entry| entry.name.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        tool_order(&first),
        vec!["aaa_tool".to_owned(), "zzz_tool".to_owned()],
        "tools settle in canonical order"
    );
    assert_eq!(
        tool_order(&second),
        tool_order(&first),
        "reversed input builds the same tool order"
    );
}
