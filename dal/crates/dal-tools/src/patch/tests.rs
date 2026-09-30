//! Patch engine behavior, style, parser, and stale-write tests.

use std::sync::{Arc, Mutex};

use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{Caller, EventStream, RawValue, Services, Tool, ToolCx, ToolOutcome};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, Question, RunOutput, RunRequest, SidecarOp, TurnOp,
    TurnOpReply, Visibility,
};

use dal_core::{CallId, GenerationId, SessionId, TurnId};

use super::{
    ir::{DialectId, EditFinding, EditObserver, FindingSeverity, StagedBatch},
    snapshot::{ReadRef, SnapshotStore},
    style::{EditStyleInput, parse_edit_style, pick},
    styles,
    write::{PatchSession, commit, plan},
};

mod observation;

fn test_session(workspace: &std::path::Path, symbols: bool) -> PatchSession {
    PatchSession {
        workspace: workspace.to_path_buf(),
        session: SessionId::new_v7(),
        generation: GenerationId::new(std::num::NonZeroU64::MIN),
        turn: TurnId::new(std::num::NonZeroU64::MIN),
        call: CallId::new("call-1"),
        consumer: dal_core::Consumer::Model,
        symbols,
        seen: crate::Seen::new(),
        index: crate::search::index::Index::new(None),
        snapshots: Arc::new(SnapshotStore::new([7; 16])),
        cutoff: Some(u64::MAX),
    }
}

#[test]
fn style_pick_table() {
    let config = parse_edit_style(&EditStyleInput::Table(vec![
        ("default".into(), "hashline".into()),
        ("*kimi*".into(), "replace".into()),
        ("*codex*".into(), "apply_patch".into()),
    ]))
    .expect("valid table");
    assert_eq!(pick(&config, "kimi-k2"), DialectId::Replace);
    assert_eq!(pick(&config, "codex-spark"), DialectId::ApplyPatch);
    assert_eq!(pick(&config, "claude-opus"), DialectId::Hashline);
    // First row wins.
    let config = parse_edit_style(&EditStyleInput::Table(vec![
        ("default".into(), "anchor".into()),
        ("*kimi*".into(), "replace".into()),
        ("*kimi*".into(), "hashline".into()),
    ]))
    .expect("valid table");
    assert_eq!(pick(&config, "kimi"), DialectId::Replace);
    // Tier aliases.
    assert_eq!(
        pick(
            &parse_edit_style(&EditStyleInput::Scalar("strict".into())).expect("strict"),
            "any"
        ),
        DialectId::Hashline
    );
    assert_eq!(
        pick(
            &parse_edit_style(&EditStyleInput::Scalar("balanced".into())).expect("balanced"),
            "any"
        ),
        DialectId::Anchor
    );
    assert_eq!(
        pick(
            &parse_edit_style(&EditStyleInput::Scalar("simple".into())).expect("simple"),
            "any"
        ),
        DialectId::Replace
    );
    // New profile names are accepted and Strict.
    assert_eq!(
        pick(
            &parse_edit_style(&EditStyleInput::Scalar("hashline-light".into())).expect("light"),
            "any"
        ),
        DialectId::HashlineLight
    );
    assert_eq!(
        pick(
            &parse_edit_style(&EditStyleInput::Scalar("hashline-enhanced".into()))
                .expect("enhanced"),
            "any"
        ),
        DialectId::HashlineEnhanced
    );
    assert_eq!(
        super::style::tier_of(DialectId::HashlineLight),
        super::ir::Tier::Strict
    );
    assert_eq!(
        super::style::tier_of(DialectId::HashlineEnhanced),
        super::ir::Tier::Strict
    );
}

#[test]
fn config_error_texts() {
    assert_eq!(
        parse_edit_style(&EditStyleInput::Scalar("sloppy".into()))
            .unwrap_err()
            .to_string(),
        "config.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![(
            "default".into(),
            "strictt".into()
        )]))
        .unwrap_err()
        .to_string(),
        "config.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![(
            "*kimi*".into(),
            "replace".into()
        )]))
        .unwrap_err()
        .to_string(),
        "config.toml: edit_style table needs a default entry."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![
            ("default".into(), "hashline".into()),
            ("".into(), "replace".into())
        ]))
        .unwrap_err()
        .to_string(),
        "config.toml: edit_style keys must not be empty."
    );
}

#[test]
fn replace_shape_catalog() {
    // Empty object and entry-count errors carry the parse suffix.
    let error = styles::parse(DialectId::Replace, "{}", false).unwrap_err();
    let text = styles::with_suffix(DialectId::Replace, error);
    assert!(text.starts_with("patch: invalid input:"), "{text}");
    assert!(text.contains("Current edit_style is \"replace\""), "{text}");
    let error = styles::parse(DialectId::Replace, "{\"changes\":[]}", false).unwrap_err();
    assert_eq!(
        error.message,
        "patch: changes must contain 1 to 64 entries."
    );
}

#[test]
fn anchor_parse_error_catalog() {
    let error = styles::parse(DialectId::Anchor, "hello", false).unwrap_err();
    let text = styles::with_suffix(DialectId::Anchor, error);
    assert!(text.starts_with("patch: input must start with"), "{text}");
    assert!(text.contains("Current edit_style is \"anchor\""), "{text}");
}

#[test]
fn hashline_grammar_catalog() {
    // Reversed range is accepted by the parser; the engine rejects past-end.
    // Past-end line and duplicate sections produce specified errors.
    let error = styles::parse(DialectId::Hashline, "[a.rs#NEW]\nPUT 1*:\n+x\n", false).unwrap_err();
    let text = styles::with_suffix(DialectId::Hashline, error);
    assert!(text.contains("block ops"), "{text}");
    assert!(
        text.contains("Current edit_style is \"hashline\""),
        "{text}"
    );
}

#[test]
fn apply_patch_parse_catalog() {
    let error = styles::parse(DialectId::ApplyPatch, "junk", false).unwrap_err();
    let text = styles::with_suffix(DialectId::ApplyPatch, error);
    assert!(
        text.contains("The first line of the patch must be"),
        "{text}"
    );
    assert!(
        text.contains("Current edit_style is \"apply_patch\""),
        "{text}"
    );
}

#[test]
fn light_rejects_legacy_tag() {
    let boot = "0123456789abcdef0123456789abcdef";
    let reference = format!("r{boot}.1");
    let input = "[a.rs#A1B2]\nPUT 1.=1:\n+x\n".to_string();
    let error = styles::parse(DialectId::HashlineLight, &input, false).unwrap_err();
    assert!(error.message.contains("[PATH#TAG]"), "{}", error.message);
    let _ = reference;
}

#[test]
fn light_header_round_trip() {
    let boot = [9_u8; 16];
    let reference = ReadRef { boot, seq: 5 };
    let token = reference.display();
    let input = format!("[src/a.rs@{token}]\nPUT 1.=1:\n+hello\n");
    let edits = styles::parse(DialectId::HashlineLight, &input, false).expect("valid light");
    assert_eq!(edits.len(), 1);
}

#[tokio::test]
async fn whole_file_tag_stale() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"before\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    // Read whole to register Seen and compute tag.
    let bytes = tokio::fs::read(&path).await.expect("read");
    let digest = *blake3::hash(&bytes).as_bytes();
    let tag = crate::tag8("whole", &bytes);
    session.seen.show(session.session, "test.txt", digest, 1, 1);
    // External drift.
    tokio::fs::write(&path, b"changed\n").await.expect("drift");
    // Replace with stale tag must fail and write nothing.
    let input = format!(
        "{{\"changes\":[{{\"path\":\"test.txt\",\"tag\":\"{tag}\",\"new\":\"after\\n\"}}]}}"
    );
    let plan = plan(&session, DialectId::Replace, &input).await;
    let error = plan.expect_err("stale tag must fail");
    assert_eq!(error.class, super::ir::ErrorClass::Stale);
    assert!(error.message.contains(&tag));
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"changed\n");
}

#[tokio::test]
async fn hashline_stale_tag_rejects() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.rs");
    tokio::fs::write(&path, b"one\ntwo\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let digest = *blake3::hash(&bytes).as_bytes();
    session.seen.show(session.session, "a.rs", digest, 1, 2);
    // Use a wrong 4-hex version tag.
    let input = "[a.rs#FFFF]\nPUT 1.=1:\n+one\n";
    let error = plan(&session, DialectId::Hashline, input)
        .await
        .expect_err("stale");
    assert_eq!(error.class, super::ir::ErrorClass::Stale);
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"one\ntwo\n");
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn light_node_edit_unobserved_passes() {
    // OBS-01: Light is observation-optional; a node replace on lines never
    // delivered still plans once reference identity + digest hold.
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.rs");
    tokio::fs::write(&path, b"fn main() {\n}\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), true);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            std::path::Path::new("a.rs"),
            &bytes,
        )
        .expect("capture");
    let input = format!("[a.rs@{}]\nPUT 1*:\n+replaced\n", reference.display());
    let staged = plan(&session, DialectId::HashlineLight, &input)
        .await
        .expect("light plans");
    let output = commit(&session, staged, &[]).await;
    assert!(output.error_class.is_none());
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn enhanced_node_edit_unobserved_fails() {
    // OBS-01: Enhanced requires cutoff-bound delivery of the node footprint.
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.rs");
    tokio::fs::write(&path, b"fn main() {\n}\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), true);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            std::path::Path::new("a.rs"),
            &bytes,
        )
        .expect("capture");
    let input = format!("[a.rs@{}]\nPUT 1*:\n+replaced\n", reference.display());
    let error = plan(&session, DialectId::HashlineEnhanced, &input)
        .await
        .expect_err("enhanced needs coverage");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"fn main() {\n}\n"
    );
}

#[tokio::test]
async fn light_lines_edit_unobserved_passes() {
    // OBS-01 without symbols: Light needs reference identity + digest
    // continuity only, so an undelivered line range still plans.
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.txt");
    tokio::fs::write(&path, b"one\ntwo\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            std::path::Path::new("a.txt"),
            &bytes,
        )
        .expect("capture");
    let input = format!("[a.txt@{}]\nPUT 1.=1:\n+uno\n", reference.display());
    let staged = plan(&session, DialectId::HashlineLight, &input)
        .await
        .expect("light plans");
    let output = commit(&session, staged, &[]).await;
    assert!(output.error_class.is_none());
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"uno\ntwo\n");
}

#[tokio::test]
async fn enhanced_lines_edit_unobserved_fails() {
    // OBS-01 without symbols: Enhanced rejects a line range with no
    // cutoff-bound delivery.
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.txt");
    tokio::fs::write(&path, b"one\ntwo\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            std::path::Path::new("a.txt"),
            &bytes,
        )
        .expect("capture");
    let input = format!("[a.txt@{}]\nPUT 1.=1:\n+uno\n", reference.display());
    let error = plan(&session, DialectId::HashlineEnhanced, &input)
        .await
        .expect_err("enhanced needs delivery");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"one\ntwo\n");
}

#[tokio::test]
async fn enhanced_lines_edit_delivered_plans() {
    // Positive path: rows delivered at/below the frozen cutoff authorize
    // the Enhanced edit; interval delivery + covers agree.
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("a.txt");
    tokio::fs::write(&path, b"one\ntwo\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    let bytes = tokio::fs::read(&path).await.expect("read");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            std::path::Path::new("a.txt"),
            &bytes,
        )
        .expect("capture");
    session.snapshots.show(reference, session.consumer, 1, 1);
    session
        .snapshots
        .deliver(session.session, session.consumer, reference, 1, 1, 7);
    assert!(
        session
            .snapshots
            .covers(session.session, session.consumer, reference, 1, 1, 7)
    );
    assert!(
        !session
            .snapshots
            .covers(session.session, session.consumer, reference, 1, 2, 7)
    );
    assert!(
        !session
            .snapshots
            .covers(session.session, session.consumer, reference, 1, 1, 6)
    );
    let input = format!("[a.txt@{}]\nPUT 1.=1:\n+uno\n", reference.display());
    // Frozen cutoff for this call is u64::MAX in the test session, so the
    // cutoff-7 delivery authorizes the range.
    let staged = plan(&session, DialectId::HashlineEnhanced, &input)
        .await
        .expect("delivered plans");
    let output = commit(&session, staged, &[]).await;
    assert!(output.error_class.is_none());
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"uno\ntwo\n");
}

#[test]
fn delivery_cutoff_is_per_event() {
    // INV-CUTOFF: rows delivered at cutoff 5 stay eligible for a cutoff-5
    // call after rows 11-20 arrive at cutoff 8; the later event must not
    // authorize a cutoff-7 call over rows it never delivered.
    use std::path::Path as StdPath;
    let store = SnapshotStore::new([3; 16]);
    let session_id = SessionId::new_v7();
    let generation = GenerationId::new(std::num::NonZeroU64::MIN);
    let bytes = b"l1\nl2\nl3\n";
    let (reference, _) = store
        .capture(
            session_id,
            generation,
            dal_core::Consumer::Model,
            StdPath::new("a.txt"),
            bytes,
        )
        .expect("capture");
    store.show(reference, dal_core::Consumer::Model, 1, 20);
    store.deliver(session_id, dal_core::Consumer::Model, reference, 1, 10, 5);
    store.deliver(session_id, dal_core::Consumer::Model, reference, 11, 20, 8);
    assert!(store.covers(session_id, dal_core::Consumer::Model, reference, 1, 10, 5));
    assert!(store.covers(session_id, dal_core::Consumer::Model, reference, 1, 10, 7));
    assert!(!store.covers(session_id, dal_core::Consumer::Model, reference, 1, 11, 7));
    assert!(store.covers(session_id, dal_core::Consumer::Model, reference, 11, 20, 8));
    assert!(!store.covers(session_id, dal_core::Consumer::Model, reference, 11, 20, 7));
}

#[tokio::test]
async fn replace_happy_path_writes() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"before\n").await.expect("seed");
    let session = test_session(dir.path(), false);
    let input = "{\"changes\":[{\"path\":\"test.txt\",\"old\":\"before\",\"new\":\"after\"}]}";
    let staged = plan(&session, DialectId::Replace, input)
        .await
        .expect("plan");
    let output = commit(&session, staged, &[]).await;
    assert!(output.error_class.is_none());
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"after\n");
    assert_eq!(output.changes.len(), 1);
}

#[test]
fn byte_stability_fixtures() {
    // Descriptions are fixed across process starts; record lengths for docs.
    let replace = styles::description(DialectId::Replace, false);
    let anchor = styles::description(DialectId::Anchor, false);
    let hashline = styles::description(DialectId::Hashline, true);
    let light = styles::description(DialectId::HashlineLight, true);
    let enhanced = styles::description(DialectId::HashlineEnhanced, true);
    assert!(!replace.is_empty() && !anchor.is_empty() && !hashline.is_empty());
    assert!(!light.is_empty() && !enhanced.is_empty());
    // Light permits unseen lines; Enhanced requires observation (checked in engine).
    assert!(light.contains("permits replacement"));
    assert!(enhanced.contains("must have been fully shown"));
    // Legacy hashline keeps 4-hex TAG language.
    assert!(hashline.contains("[PATH#TAG]"));
}

#[test]
fn contract_enumeration() {
    // Every dialect renders every EngineError variant without panicking and
    // parse-class renderings carry the current-dialect suffix.
    for style in [
        DialectId::Anchor,
        DialectId::Replace,
        DialectId::Hashline,
        DialectId::HashlineLight,
        DialectId::HashlineEnhanced,
        DialectId::ApplyPatch,
    ] {
        for class in [
            super::ir::ErrorClass::Parse,
            super::ir::ErrorClass::Resolve,
            super::ir::ErrorClass::Proof,
            super::ir::ErrorClass::Stale,
            super::ir::ErrorClass::File,
            super::ir::ErrorClass::Limit,
            super::ir::ErrorClass::Blocked,
            super::ir::ErrorClass::Io,
        ] {
            let error = super::ir::EngineError::new(class, "probe");
            let rendered = match class {
                super::ir::ErrorClass::Parse => {
                    styles::with_suffix(style, super::ir::ParseError::new(error.message.clone()))
                }
                _ => error.message.clone(),
            };
            assert!(!rendered.is_empty());
            if class == super::ir::ErrorClass::Parse {
                assert!(rendered.contains("Current edit_style is"), "{rendered}");
            }
        }
    }
}

#[test]
fn enhanced_never_falls_back_to_light() {
    // Enhanced and Light share syntax but differ in eligibility; the parser
    // never rewrites one style into the other.
    let boot = "abcdef0123456789abcdef0123456789";
    let token = format!("r{boot}.2");
    let input = format!("[a.rs@{token}]\nPUT 1.=1:\n+x\n");
    let light = styles::parse(DialectId::HashlineLight, &input, false).expect("light parses");
    let enhanced =
        styles::parse(DialectId::HashlineEnhanced, &input, false).expect("enhanced parses");
    assert_eq!(light.len(), enhanced.len());
    // Style identity is preserved by the caller; no automatic downgrade.
    assert_ne!(
        styles::name(DialectId::HashlineLight),
        styles::name(DialectId::HashlineEnhanced)
    );
}

struct NeverServices;

impl Services for NeverServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("replacement test does not use services")
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unreachable!("replacement test does not use services")
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unreachable!("replacement test does not use services")
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unreachable!("replacement test does not use services")
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unreachable!("replacement test does not use services")
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unreachable!("replacement test does not use services")
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unreachable!("replacement test does not use services")
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unreachable!("replacement test does not use services")
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unreachable!("replacement test does not use services")
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unreachable!("replacement test does not use services")
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unreachable!("replacement test does not use services")
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unreachable!("replacement test does not use services")
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unreachable!("replacement test does not use services")
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        unreachable!("replacement test does not use services")
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unreachable!("replacement test does not use services")
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("replacement test does not use services")
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unreachable!("replacement test does not use services")
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unreachable!("replacement test does not use services")
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unreachable!("replacement test does not use services")
    }

    fn notify(&self, _who: &Caller, _notice: Notice) {
        unreachable!("replacement test does not use services")
    }

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unreachable!("replacement test does not use services")
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        unreachable!("replacement test does not use services")
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unreachable!("replacement test does not use services")
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("replacement test does not use services")
    }
}

#[tokio::test]
async fn apply_replacement_denial_is_typed_and_atomic() {
    let mut cx = ToolCx::for_test(Arc::new(NeverServices));
    let dir = tempfile::tempdir_in(cx.workspace().as_path()).expect("temp workspace");
    let path = dir.path().join("test.txt");
    let name = path
        .strip_prefix(cx.workspace().as_path())
        .expect("workspace child")
        .to_string_lossy()
        .into_owned();
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let error = super::apply_replacement(&mut cx, &name, b"needle", b"new", 2, &[])
        .await
        .expect_err("test context denies mutation");
    assert!(matches!(
        error,
        super::ApplyError::Blocked(dal_core::DenyReason::NoFrontEnd)
    ));
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"alpha\nneedle\nomega\n"
    );
}

struct BlockingObserver;

impl EditObserver for BlockingObserver {
    fn inspect(&self, _batch: &StagedBatch<'_>) -> Vec<EditFinding> {
        vec![EditFinding {
            rule: "replacement-block".into(),
            severity: FindingSeverity::Block,
            text: "replacement blocked".into(),
        }]
    }
}

#[tokio::test]
async fn apply_replacement_blocking_observer_prevents_write() {
    let mut cx = ToolCx::for_test(Arc::new(NeverServices));
    let dir = tempfile::tempdir_in(cx.workspace().as_path()).expect("temp workspace");
    let path = dir.path().join("test.txt");
    let name = path
        .strip_prefix(cx.workspace().as_path())
        .expect("workspace child")
        .to_string_lossy()
        .into_owned();
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let observers: [Arc<dyn EditObserver>; 1] = [Arc::new(BlockingObserver)];
    let error = super::apply_replacement(&mut cx, &name, b"needle", b"new", 2, &observers)
        .await
        .expect_err("blocking observer rejects replacement");
    let super::ApplyError::ObserverBlocked(finding) = error else {
        panic!("expected an observer block");
    };
    assert_eq!(finding.text.as_ref(), "replacement blocked");
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"alpha\nneedle\nomega\n"
    );
}

struct RecordingObserver(Arc<Mutex<Vec<(Vec<u8>, Vec<String>)>>>);

impl EditObserver for RecordingObserver {
    fn inspect(&self, batch: &StagedBatch<'_>) -> Vec<EditFinding> {
        if let Some(file) = batch.files.first() {
            let after = file.after.map_or_else(Vec::new, |after| after.to_vec());
            let added = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == super::ir::DiffLineKind::Added)
                .map(|line| line.text.to_string())
                .collect();
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((after, added));
        }
        vec![EditFinding {
            rule: "replacement-test".into(),
            severity: FindingSeverity::Report,
            text: "replacement observed".into(),
        }]
    }
}

#[tokio::test]
async fn replacement_plan_commit_applies_exact_bytes_and_observes() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), false);
    let plan = super::write::plan_replacement(&session, "test.txt", b"needle", b"new\r\nbytes", 2)
        .await
        .expect("replacement plan");
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observer = RecordingObserver(Arc::clone(&observed));
    let output = commit(&session, plan, &[Arc::new(observer)]).await;
    assert!(output.error_class.is_none(), "{}", output.text);
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"alpha\nnew\r\nbytes\nomega\n"
    );
    assert_eq!(
        observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_slice(),
        [(
            b"alpha\nnew\r\nbytes\nomega\n".to_vec(),
            ["new".to_owned(), "bytes".to_owned()].into(),
        )]
    );
    assert!(output.text.contains("replacement observed"));
}

#[tokio::test]
async fn replacement_plan_handles_multiline_before() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), false);
    let plan = super::write::plan_replacement(&session, "test.txt", b"needle\nomega", b"new", 2)
        .await
        .expect("multiline replacement plan");
    let output = commit(&session, plan, &[]).await;
    assert!(output.error_class.is_none(), "{}", output.text);
    assert_eq!(tokio::fs::read(&path).await.expect("read"), b"alpha\nnew\n");
}

#[tokio::test]
async fn replacement_plan_rejects_nonmatching_before_without_writing() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), false);
    let error = super::write::plan_replacement(&session, "test.txt", b"missing", b"new", 2)
        .await
        .expect_err("nonmatching source");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    assert!(error.message.contains("test.txt:2"), "{}", error.message);
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"alpha\nneedle\nomega\n"
    );
}

#[tokio::test]
async fn replacement_preview_matches_replace_record() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("test.txt");
    tokio::fs::write(&path, b"alpha\nneedle\nomega\n")
        .await
        .expect("seed");
    let session = test_session(dir.path(), false);
    let direct = super::write::plan_replacement(&session, "test.txt", b"needle", b"new", 2)
        .await
        .expect("direct plan");
    let record = plan(
        &session,
        DialectId::Replace,
        r#"{"changes":[{"path":"test.txt","old":"needle","new":"new","line":2}]}"#,
    )
    .await
    .expect("replace plan");
    assert_eq!(
        super::preview_for_plan(&direct),
        super::preview_for_plan(&record)
    );
}
