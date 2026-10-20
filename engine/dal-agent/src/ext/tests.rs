//! Builder, record, model, and status API unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dal_core::ext::{ExportId, ExportKind, OpSet};
use dal_core::{
    Caps, Claimant, ContextItem, DenyReason, Inference, ModelId, ModelRequest, ModelRoute,
    ModelToolSpec, Name, Origin, Purpose, RawJson, RegistrationError, RequestParams, RuleRecord,
    ScopeSpec, ServiceSet, SkillRecord,
};
use dal_provider::EventStream;
use tokio_util::sync::CancellationToken;

use super::{
    BoxFuture, Caller, ExportSpec, ExtensionBuilder, ModelCx, ModelCxRuntime, ModelError,
    ModelHandler, ModelRecord, PromptSection, Scope, ScopeError, SessionInstalls, StatusCx,
    StatusPoll, StatusSnapshot,
};

fn builder() -> ExtensionBuilder {
    ExtensionBuilder::new("focus", "1.2.3", ServiceSet::EMPTY).expect("valid builder")
}

fn skill(name: &str) -> SkillRecord {
    SkillRecord {
        name: name.parse::<Name>().expect("valid skill name"),
        description: "test skill".into(),
        body: "body".into(),
        letter2image: false,
        mcp: None,
    }
}

fn caps() -> Caps {
    Caps {
        context_window: Some(8192),
        thinking: Box::default(),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: ModelRoute::Synthetic {
            id: "acme/test".into(),
        },
        system: "".into(),
        tools: Vec::<ModelToolSpec>::new().into(),
        context: Vec::<ContextItem>::new().into(),
        params: RequestParams::default(),
        cache_key: None,
    }
}

struct Noisy;

impl StatusPoll for Noisy {
    fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
        StatusSnapshot {
            quiet: false,
            text: Some("loud".into()),
        }
    }
}

struct TestHandler;

impl ModelHandler for TestHandler {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { Err(ModelError::PrivateRounds) })
    }
}

struct ScriptModelCx {
    calls: AtomicUsize,
    forwarded: AtomicBool,
}

impl ModelCxRuntime for ScriptModelCx {
    fn scope(&self, _spec: ScopeSpec) -> Result<Scope, ScopeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ScopeError::Denied(DenyReason::Unavailable {
            what: "test scope".into(),
        }))
    }

    fn infer<'a>(
        &'a self,
        _who: &'a Caller,
        _request: ModelRequest,
        _script: Arc<crate::session::script::SessionScriptHost>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Inference, crate::error::ServiceError>> {
        Box::pin(async { Err(crate::error::ServiceError::Denied(DenyReason::NotInjected)) })
    }

    fn script_forward<'a>(
        &'a self,
        _who: &'a Caller,
        _request: ModelRequest,
        _private: &'a [super::PrivateTool],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EventStream, crate::error::ServiceError>> {
        Box::pin(async { Err(crate::error::ServiceError::Denied(DenyReason::NotInjected)) })
    }

    fn forward<'a>(
        &'a self,
        _request: ModelRequest,
        _private: &'a [super::PrivateTool],
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        if self.forwarded.swap(true, Ordering::SeqCst) {
            return Box::pin(async { Err(ModelError::SecondForward) });
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ModelError::PrivateRounds) })
    }
}

#[test]
fn builder_rejects_duplicate_records() {
    let err = builder()
        .skill(skill("dup"))
        .skill(skill("dup"))
        .build()
        .expect_err("duplicate");
    match err {
        RegistrationError::Conflict {
            kind,
            name,
            claimant,
            rival,
        } => {
            assert_eq!(kind, "skill");
            assert_eq!(name.as_str(), "dup");
            assert!(matches!(&claimant, Claimant::Plugin(_)));
            assert_eq!(rival, claimant);
        }
        other => panic!("expected conflict, got {other:?}"),
    }
}

#[test]
fn builder_accepts_distinct_prompt_orders_and_keeps_other_registrations() {
    let ext = builder()
        .skill(skill("ordered"))
        .prompt_section(PromptSection::static_text(
            super::PromptOrder::Identity,
            "identity".into(),
        ))
        .prompt_section(PromptSection::static_text(
            super::PromptOrder::Instructions,
            "instructions".into(),
        ))
        .prompt_section(PromptSection::static_text(
            super::PromptOrder::DocsLine,
            "docs".into(),
        ))
        .build()
        .expect("distinct orders accepted");
    let sections = ext.prompt_sections();
    assert_eq!(
        sections
            .iter()
            .map(PromptSection::order)
            .collect::<Vec<_>>(),
        vec![
            super::PromptOrder::Identity,
            super::PromptOrder::Instructions,
            super::PromptOrder::DocsLine,
        ],
        "registration order is preserved"
    );
    assert_eq!(
        ext.prompt_section().map(PromptSection::order),
        Some(super::PromptOrder::Identity),
        "singular accessor keeps the first section"
    );
    assert_eq!(ext.skills().len(), 1, "other registrations survive");
}

#[test]
fn builder_rejects_duplicate_prompt_order() {
    let err = builder()
        .prompt_section(PromptSection::static_text(
            super::PromptOrder::Instructions,
            "a".into(),
        ))
        .prompt_section(PromptSection::static_text(
            super::PromptOrder::Instructions,
            "b".into(),
        ))
        .build()
        .expect_err("second section at the same order");
    assert!(matches!(err, RegistrationError::DuplicatePromptSection));
}
#[test]
fn builder_rejects_duplicate_session_setup() {
    let setup =
        |_cx: super::SessionCx| -> BoxFuture<'static, Result<SessionInstalls, super::HookError>> {
            Box::pin(async { Ok(SessionInstalls::default()) })
        };
    let err = builder()
        .session_setup(setup)
        .session_setup(setup)
        .build()
        .expect_err("duplicate session setup");
    assert!(matches!(
        err,
        RegistrationError::Conflict {
            kind: "session_setup",
            ..
        }
    ));
}

#[test]
fn builder_rejects_second_status_kind() {
    let err = builder()
        .status_kind("a", Arc::new(Noisy))
        .status_kind("b", Arc::new(Noisy))
        .build()
        .expect_err("second status");
    let RegistrationError::DuplicateStatusKind { ext } = err else {
        panic!("expected duplicate status kind, got {err:?}");
    };
    assert_eq!(ext.as_str(), "focus");
}

#[test]
fn builder_accepts_valid_synthetic_model_id() {
    let id = ModelId::parse("acme/model-1.0").expect("valid synthetic id");
    let handler: Arc<dyn ModelHandler> = Arc::new(TestHandler);
    let ext = builder()
        .with_origin(Origin::Bundled, None)
        .model(ModelRecord {
            id: id.clone(),
            caps: caps(),
            handler: handler.clone(),
            export: None,
        })
        .build()
        .expect("valid model");
    assert_eq!(ext.models().len(), 1);
    assert_eq!(ext.models()[0].id.as_str(), "acme/model-1.0");
    assert_eq!(ext.models()[0].caps, caps());
    assert!(Arc::ptr_eq(&ext.models()[0].handler, &handler));
    assert_eq!(ext.origin(), Origin::Bundled);
}

#[test]
fn synthetic_model_id_grammar_holds_at_every_edge() {
    for id in ["a/b", "acme/model-1.0", "0-9/x._-", "ac-me/under_score"] {
        assert_eq!(
            ModelId::parse(id)
                .expect("a valid spelling parses")
                .as_str(),
            id
        );
    }
    for id in [
        "not a model id!",
        "",
        "acme",
        "acme/",
        "/model",
        "a/b/c",
        "Acme/model",
        "acme/Model",
        "ac_me/model",
        "acme/mo del",
    ] {
        let Err(RegistrationError::InvalidModelId { id: rejected }) = ModelId::parse(id) else {
            panic!("{id:?} must be an invalid synthetic model id");
        };
        assert_eq!(&*rejected, id, "the rejection echoes the offending id");
    }
}

#[test]
fn model_scope_delegates_to_its_runtime() {
    let rt = Arc::new(ScriptModelCx {
        calls: AtomicUsize::new(0),
        forwarded: AtomicBool::new(false),
    });
    let cx = ModelCx::new(rt.clone());
    let outcome = cx.scope(ScopeSpec {
        limit: 1,
        on_error: dal_core::OnError::Settle,
        budget: dal_core::Budget::default(),
    });
    assert!(matches!(
        outcome,
        Err(ScopeError::Denied(DenyReason::Unavailable { .. }))
    ));
    assert_eq!(rt.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn forward_is_single_use() {
    let rt = Arc::new(ScriptModelCx {
        calls: AtomicUsize::new(0),
        forwarded: AtomicBool::new(false),
    });
    let cx = ModelCx::new(rt.clone());
    let first = futures::executor::block_on(cx.forward(request(), &[]));
    assert!(matches!(first, Err(ModelError::PrivateRounds)));
    assert_eq!(rt.calls.load(Ordering::SeqCst), 1);
    let second = futures::executor::block_on(cx.forward(request(), &[]));
    assert!(matches!(second, Err(ModelError::SecondForward)));
    assert_eq!(rt.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn builder_rejects_reserved_doc_scheme_with_call_site() {
    for name in ["job", "rule"] {
        let err = ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
            .expect("valid builder")
            .doc("index", "Title", "pages")
            .build()
            .expect_err("reserved scheme");
        let text = err.to_string();
        assert!(
            text.contains(&format!("doc scheme '{name}' is reserved")),
            "{text}"
        );
        assert!(
            text.contains("tests.rs:"),
            "error carries path:line:col: {text}"
        );
        assert!(matches!(err, RegistrationError::DocSchemeReserved { .. }));
    }
}

#[test]
fn builder_rejects_doc_scheme_outside_scheme_grammar() {
    for name in ["my_plugin", "a_b-c", "trailing_"] {
        let err = ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
            .expect("valid builder")
            .doc("index", "Title", "text")
            .build()
            .expect_err("scheme grammar");
        assert!(
            matches!(err, RegistrationError::InvalidDocScheme { .. }),
            "{name}: {err}"
        );
    }
}

/// One export declaration with an empty `uses` set.
fn export(plugin: &str, kind: ExportKind, local: &str) -> ExportSpec {
    ExportSpec {
        id: ExportId {
            plugin: plugin.parse().expect("plugin name"),
            kind,
            local: local.parse().expect("local name"),
        },
        uses: OpSet::EMPTY,
        input: RawJson::parse(r#"{"type":"object"}"#).expect("schema"),
        description: "test export".into(),
    }
}

/// Requires `result` to be an invalid-export rejection naming `id` and `wire`.
fn assert_invalid_export(
    result: Result<super::Extension, RegistrationError>,
    id: &str,
    wire: &str,
) {
    match result {
        Err(RegistrationError::InvalidExport {
            id: rejected,
            wire: required,
        }) => {
            assert_eq!(&*rejected, id, "the rejection names the export");
            assert_eq!(&*required, wire, "the rejection names the wire name");
        }
        Err(other) => panic!("expected an invalid export for {id}, got {other:?}"),
        Ok(_) => panic!("the export {id} must be rejected"),
    }
}

#[test]
fn builder_identity_grammar_holds_at_both_edges() {
    let longest = "a".repeat(64);
    for name in ["a", "a1", "a-b_c", longest.as_str()] {
        assert!(
            ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY).is_ok(),
            "{name:?} is a valid extension name"
        );
    }
    let too_long = "a".repeat(65);
    for name in [
        "",
        "1abc",
        "Abc",
        "aBc",
        "_a",
        "-a",
        "a b",
        "a.b",
        "a/b",
        "a\u{e9}",
        too_long.as_str(),
    ] {
        let Err(RegistrationError::InvalidName { name: rejected }) =
            ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
        else {
            panic!("{name:?} must be an invalid extension name");
        };
        assert_eq!(&*rejected, name, "the rejection echoes the offending name");
    }
}

#[test]
fn builder_identity_checks_the_name_before_the_version() {
    let Err(RegistrationError::InvalidVersion { version }) =
        ExtensionBuilder::new("focus", "1.2", ServiceSet::EMPTY)
    else {
        panic!("a two-part version must be invalid");
    };
    assert_eq!(&*version, "1.2");
    assert!(ExtensionBuilder::new("focus", "1.2.3-rc.1+build.5", ServiceSet::EMPTY).is_ok());
    assert!(
        matches!(
            ExtensionBuilder::new("Focus", "1.2", ServiceSet::EMPTY),
            Err(RegistrationError::InvalidName { .. })
        ),
        "a bad name wins over a bad version"
    );
}

#[test]
fn builder_rejects_duplicate_rules_but_lets_a_skill_and_a_rule_share_a_name() {
    let rule = |name: &str| RuleRecord {
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
    };
    let err = builder()
        .rule(rule("dup"))
        .rule(rule("dup"))
        .build()
        .expect_err("two rules with one name");
    let RegistrationError::Conflict { kind, name, .. } = err else {
        panic!("expected a conflict, got {err:?}");
    };
    assert_eq!(kind, "rule");
    assert_eq!(name.as_str(), "dup");

    let ext = builder()
        .skill(skill("shared"))
        .rule(rule("shared"))
        .build()
        .expect("a skill and a rule may share a name");
    assert_eq!(ext.skills().len(), 1);
    assert_eq!(ext.rules().len(), 1);
}

#[test]
fn a_hook_export_of_another_plugin_is_rejected() {
    assert_invalid_export(
        builder()
            .hook_export(export("other", ExportKind::Hook, "audit"))
            .build(),
        "hooks.other.audit",
        "other__audit",
    );
    let ext = builder()
        .hook_export(export("focus", ExportKind::Hook, "audit"))
        .build()
        .expect("an export of the extension's own plugin is accepted");
    let [(spec, wire)] = ext.exports() else {
        panic!("one export registers: {:?}", ext.exports().len());
    };
    assert_eq!(spec.id.local.as_str(), "audit");
    assert_eq!(wire.as_str(), "focus__audit");
}

#[test]
fn declared_exports_are_commands_or_hooks_only() {
    assert_invalid_export(
        builder()
            .hook_export(export("focus", ExportKind::Tool, "t"))
            .build(),
        "tools.focus.t",
        "focus__t",
    );
    assert_invalid_export(
        builder()
            .hook_export(export("focus", ExportKind::Model, "m"))
            .build(),
        "models.focus.m",
        "focus__m",
    );
    let ext = builder()
        .hook_export(export("focus", ExportKind::Command, "run"))
        .hook_export(export("focus", ExportKind::Hook, "run"))
        .build()
        .expect("a command and a hook may share a local name");
    assert_eq!(ext.exports().len(), 2);
}

#[test]
fn a_declared_export_identity_registers_once() {
    assert_invalid_export(
        builder()
            .hook_export(export("focus", ExportKind::Hook, "audit"))
            .hook_export(export("focus", ExportKind::Hook, "audit"))
            .build(),
        "hooks.focus.audit",
        "focus__audit",
    );
}

#[test]
fn a_model_export_is_this_plugins_model_kind_and_unique() {
    let model = |id: &str, export: ExportId| ModelRecord {
        id: ModelId::parse(id).expect("valid model id"),
        caps: caps(),
        handler: Arc::new(TestHandler),
        export: Some(export),
    };
    let export_id = |plugin: &str, kind: ExportKind, local: &str| ExportId {
        plugin: plugin.parse().expect("plugin name"),
        kind,
        local: local.parse().expect("local name"),
    };
    assert_invalid_export(
        builder()
            .model(model(
                "acme/m1",
                export_id("other", ExportKind::Model, "route"),
            ))
            .build(),
        "models.other.route",
        "other__route",
    );
    assert_invalid_export(
        builder()
            .model(model(
                "acme/m1",
                export_id("focus", ExportKind::Tool, "route"),
            ))
            .build(),
        "tools.focus.route",
        "focus__route",
    );
    assert_invalid_export(
        builder()
            .model(model(
                "acme/m1",
                export_id("focus", ExportKind::Model, "route"),
            ))
            .model(model(
                "acme/m2",
                export_id("focus", ExportKind::Model, "route"),
            ))
            .build(),
        "models.focus.route",
        "focus__route",
    );
    builder()
        .model(model(
            "acme/m1",
            export_id("focus", ExportKind::Model, "route"),
        ))
        .build()
        .expect("a model export of the extension's own plugin registers");
}

#[test]
fn builder_rejects_doc_paths_outside_page_grammar() {
    for path in ["", "CLI", "a/", "a//b", "../x", "a b", "a_B"] {
        let err = builder()
            .doc(path, "Title", "text")
            .build()
            .expect_err("page grammar");
        assert!(
            matches!(err, RegistrationError::InvalidDocPage { .. }),
            "{path:?}: {err}"
        );
    }
    for path in ["plugins", "convert-pi", "examples/hello", "a1/b-2/c3"] {
        builder()
            .doc(path, "Title", "text")
            .build()
            .expect("valid page");
    }
}
