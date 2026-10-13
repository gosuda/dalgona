//! Builder, record, model, and status API unit tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dal_core::{
    Caps, Claimant, ContextItem, DenyReason, Inference, ModelId, ModelRequest, ModelRoute,
    ModelToolSpec, Name, Origin, Purpose, RegistrationError, RequestParams, ScopeSpec, ServiceSet,
    SkillRecord,
};
use dal_provider::EventStream;
use tokio_util::sync::CancellationToken;

use super::{
    BoxFuture, Caller, ExtensionBuilder, ModelCx, ModelCxRuntime, ModelError, ModelHandler,
    ModelRecord, PromptSection, Scope, ScopeError, StatusCx, StatusPoll, StatusSnapshot,
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
        } => {
            assert_eq!(kind, "skill");
            assert_eq!(name.as_str(), "dup");
            assert!(matches!(claimant, Claimant::Plugin(_)));
        }
        other => panic!("expected conflict, got {other:?}"),
    }
}

#[test]
fn builder_rejects_second_prompt_section() {
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
        .expect_err("second section");
    assert!(matches!(err, RegistrationError::DuplicatePromptSection));
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
fn builder_rejects_invalid_synthetic_model_id() {
    let err = ModelId::parse("not a model id!").expect_err("invalid id");
    assert!(matches!(err, RegistrationError::InvalidModelId { .. }));
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
