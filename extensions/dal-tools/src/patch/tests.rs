//! Patch engine behavior, style, parser, and stale-write tests.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, EventStream, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, Question, RunOutput, RunRequest, SidecarOp, StateError,
    StateOp, StateRecord, TurnOp, TurnOpReply, Visibility,
};

use dal_core::{CallId, GenerationId, SessionId, ToolClass, TurnId};

use super::{
    ir::{
        Action, DialectId, Edit, EditFinding, EditObserver, FindingSeverity, Guard, Locator,
        Operation, StagedBatch, Window,
    },
    snapshot::{ReadRef, SnapshotStore},
    style::{EditStyleInput, parse_edit_style, pick},
    styles,
    write::{PatchSession, commit, plan, stage},
};

mod observation;
mod rename_containment;

fn change_edit(path: &str, locator: Locator, action: Action, body: &str) -> Edit {
    change_edit_guard(path, locator, action, Guard::Quoted, body)
}

fn change_edit_guard(
    path: &str,
    locator: Locator,
    action: Action,
    guard: Guard,
    body: &str,
) -> Edit {
    Edit::Change {
        index: 0,
        path: std::path::PathBuf::from(path),
        locator,
        action,
        guard,
        body: body.to_owned(),
        window: Window::BeforePayload,
    }
}

async fn stage_one(
    session: &PatchSession,
    path: &std::path::Path,
    bytes: &[u8],
    edits: Vec<Edit>,
) -> Result<super::ir::StagedFileOwned, super::ir::EngineError> {
    let canonical = session.workspace.join(path);
    tokio::fs::write(&canonical, bytes)
        .await
        .expect("seed the target file");
    stage::stage_file(session, DialectId::Replace, path, &canonical, edits).await
}

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
        "dal.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![(
            "default".into(),
            "strictt".into()
        )]))
        .unwrap_err()
        .to_string(),
        "dal.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![(
            "*kimi*".into(),
            "replace".into()
        )]))
        .unwrap_err()
        .to_string(),
        "dal.toml: edit_style table needs a default entry."
    );
    assert_eq!(
        parse_edit_style(&EditStyleInput::Table(vec![
            ("default".into(), "hashline".into()),
            ("".into(), "replace".into())
        ]))
        .unwrap_err()
        .to_string(),
        "dal.toml: edit_style keys must not be empty."
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
fn hashline_accepts_omitted_final_line_break() {
    let with_break = styles::parse(DialectId::Hashline, "[a.rs#NEW]\nPUT >$:\n+body\n", false)
        .expect("terminated payload parses");
    let without_break = styles::parse(DialectId::Hashline, "[a.rs#NEW]\nPUT >$:\n+body", false)
        .expect("omitted final line break parses");
    assert_eq!(with_break.len(), 1);
    assert_eq!(without_break.len(), 1);
    let Edit::Create {
        body: terminated, ..
    } = &with_break[0]
    else {
        panic!("terminated payload creates, got {:?}", with_break[0]);
    };
    let Edit::Create {
        body: unterminated, ..
    } = &without_break[0]
    else {
        panic!("unterminated payload creates, got {:?}", without_break[0]);
    };
    assert_eq!(terminated, "body\n");
    assert_eq!(unterminated, "body");
    let multi = "[a.rs#NEW]\nPUT >$:\n+first\n+}";
    let edits = styles::parse(DialectId::Hashline, multi, false).expect("multi-row tail parses");
    let [Edit::Create { body, .. }] = edits.as_slice() else {
        panic!("multi-row payload creates one file, got {edits:?}");
    };
    assert_eq!(body, "first\n}");
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
    let token = format!("r{boot}.1");
    // The same shape with a real reference token must parse: rejection is
    // about the legacy form, not about every HashlineLight header.
    let valid = format!("[a.rs@{token}]\nPUT 1.=1:\n+x\n");
    let edits =
        styles::parse(DialectId::HashlineLight, &valid, false).expect("real reference parses");
    assert_eq!(edits.len(), 1);
    let input = "[a.rs#A1B2]\nPUT 1.=1:\n+x\n".to_string();
    let error = styles::parse(DialectId::HashlineLight, &input, false).unwrap_err();
    assert!(error.message.contains("[PATH#TAG]"), "{}", error.message);
}

#[test]
fn light_header_round_trip() {
    let boot = [9_u8; 16];
    let reference = ReadRef { boot, seq: 5 };
    let token = reference.display();
    let input = format!("[src/a.rs@{token}]\nPUT 1.=1:\n+hello\n");
    let edits = styles::parse(DialectId::HashlineLight, &input, false).expect("valid light");
    assert_eq!(edits.len(), 1);
    // The header binds path, proof, and body: dropping the reference token
    // while still returning one edit must turn this test red.
    let Edit::Change {
        path,
        locator,
        action,
        guard,
        body,
        ..
    } = &edits[0]
    else {
        panic!("light PUT parses to a change, got {:?}", edits[0]);
    };
    assert_eq!(*path, std::path::PathBuf::from("src/a.rs"));
    assert_eq!(*locator, Locator::Lines { first: 1, last: 1 });
    assert_eq!(*action, Action::Replace);
    assert_eq!(*guard, Guard::Reference(token.to_string()));
    assert_eq!(*body, "hello\n");
    assert_eq!(ReadRef::parse(&token), Some(reference));
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
    // Rows delivered at cutoff 5 stay eligible for a cutoff-5
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
fn error_class_wire_names_are_the_published_lowercase_words() {
    use super::ir::ErrorClass;
    let table = [
        (ErrorClass::Parse, "\"parse\""),
        (ErrorClass::Resolve, "\"resolve\""),
        (ErrorClass::Proof, "\"proof\""),
        (ErrorClass::Stale, "\"stale\""),
        (ErrorClass::File, "\"file\""),
        (ErrorClass::Limit, "\"limit\""),
        (ErrorClass::Blocked, "\"blocked\""),
        (ErrorClass::Io, "\"io\""),
    ];
    for (class, wire) in table {
        assert_eq!(
            sonic_rs::to_string(&class).expect("class serializes"),
            wire,
            "{class:?}"
        );
    }
}

/// Every dialect with the exact name its parse errors must carry.
const DIALECT_NAMES: [(DialectId, &str); 6] = [
    (DialectId::Anchor, "anchor"),
    (DialectId::Replace, "replace"),
    (DialectId::Hashline, "hashline"),
    (DialectId::HashlineLight, "hashline-light"),
    (DialectId::HashlineEnhanced, "hashline-enhanced"),
    (DialectId::ApplyPatch, "apply_patch"),
];

#[test]
fn parse_suffix_names_exactly_the_current_dialect() {
    for (style, name) in DIALECT_NAMES {
        let text = styles::with_suffix(style, super::ir::ParseError::new("probe"));
        assert_eq!(
            text,
            format!(
                "probe\nCurrent edit_style is \"{name}\" for this model; past calls in history may use another dialect. Do not imitate them."
            ),
            "{style:?}"
        );
    }
}

#[tokio::test]
async fn every_dialect_rejects_a_junk_payload_naming_itself() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    for (style, name) in DIALECT_NAMES {
        let Err(error) = plan(&session, style, "junk").await else {
            panic!("{style:?} must reject a junk payload");
        };
        assert_eq!(error.class, super::ir::ErrorClass::Parse, "{style:?}");
        let suffix = format!(
            "\nCurrent edit_style is \"{name}\" for this model; past calls in history may use another dialect. Do not imitate them."
        );
        assert!(
            error.message.ends_with(&suffix),
            "{style:?}: {}",
            error.message
        );
        assert_eq!(
            error.message.matches("Current edit_style is").count(),
            1,
            "{style:?}: {}",
            error.message
        );
    }
}

#[test]
fn enhanced_never_falls_back_to_light() {
    // Enhanced and Light share syntax but differ in eligibility: both parses
    // must carry the same reference proof, and the style identity the caller
    // dispatches on must stay distinct. Engine eligibility itself is pinned
    // by light_lines_edit_unobserved_passes and
    // enhanced_lines_edit_unobserved_fails.
    let boot = "abcdef0123456789abcdef0123456789";
    let token = format!("r{boot}.2");
    let input = format!("[a.rs@{token}]\nPUT 1.=1:\n+x\n");
    let light = styles::parse(DialectId::HashlineLight, &input, false).expect("light parses");
    let enhanced =
        styles::parse(DialectId::HashlineEnhanced, &input, false).expect("enhanced parses");
    assert_eq!(light.len(), enhanced.len());
    assert_eq!(light, enhanced);
    let Edit::Change {
        path, guard, body, ..
    } = &light[0]
    else {
        panic!("shared syntax parses to a change, got {:?}", light[0]);
    };
    assert_eq!(*path, std::path::PathBuf::from("a.rs"));
    assert_eq!(*guard, Guard::Reference(token));
    assert_eq!(*body, "x\n");
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

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
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

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unreachable!("replacement test does not use services")
    }

    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
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

type Recorded = (Vec<u8>, Vec<String>);

struct RecordingObserver(Arc<Mutex<Vec<Recorded>>>);

impl EditObserver for RecordingObserver {
    fn inspect(&self, batch: &StagedBatch<'_>) -> Vec<EditFinding> {
        if let Some(file) = batch.files.first() {
            let after = file.after.map_or_else(Vec::new, <[u8]>::to_vec);
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
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let observer = RecordingObserver(Arc::clone(&recorded));
    let output = commit(&session, plan, &[Arc::new(observer)]).await;
    assert!(output.error_class.is_none(), "{}", output.text);
    assert_eq!(
        tokio::fs::read(&path).await.expect("read"),
        b"alpha\nnew\r\nbytes\nomega\n"
    );
    assert_eq!(
        recorded
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

#[tokio::test]
async fn stage_lines_actions_and_invalid_ranges() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    for (action, expected) in [
        (Action::Replace, "a\nX\nd\n"),
        (Action::InsertBefore, "a\nX\nb\nc\nd\n"),
        (Action::InsertAfter, "a\nb\nc\nX\nd\n"),
    ] {
        let staged = stage_one(
            &session,
            path,
            b"a\nb\nc\nd\n",
            vec![change_edit(
                "a.txt",
                Locator::Lines { first: 2, last: 3 },
                action,
                "X\n",
            )],
        )
        .await
        .expect("lines edit plans");
        assert_eq!(&*staged.after.expect("after"), expected.as_bytes());
        assert_eq!(staged.op, Operation::Update);
        assert!(!staged.hunks.is_empty(), "replace dialect emits hunks");
    }
    for (first, last) in [(0, 1), (3, 2), (1, 9)] {
        let error = stage_one(
            &session,
            path,
            b"a\nb\nc\nd\n",
            vec![change_edit(
                "a.txt",
                Locator::Lines { first, last },
                Action::Replace,
                "X\n",
            )],
        )
        .await
        .expect_err("invalid range rejects");
        assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    }
}

#[tokio::test]
async fn stage_gap_inserts_and_bounds() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    for (before_line, expected) in [
        (0_usize, "X\na\nb\n"),
        (2, "a\nX\nb\n"),
        (usize::MAX, "a\nb\nX\n"),
    ] {
        let staged = stage_one(
            &session,
            path,
            b"a\nb\n",
            vec![change_edit(
                "a.txt",
                Locator::Gap { before_line },
                Action::InsertAfter,
                "X\n",
            )],
        )
        .await
        .expect("gap plans");
        assert_eq!(&*staged.after.expect("after"), expected.as_bytes());
    }
    let error = stage_one(
        &session,
        path,
        b"a\nb\n",
        vec![change_edit(
            "a.txt",
            Locator::Gap { before_line: 9 },
            Action::InsertAfter,
            "X\n",
        )],
    )
    .await
    .expect_err("past-end gap rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
}

#[tokio::test]
async fn stage_text_unique_ambiguous_hinted_and_all() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let text_locator = |all: bool, line_hint: Option<usize>| Locator::Text {
        old: "x".to_owned(),
        line_hint,
        all,
        window: Window::BeforePayload,
        context: None,
        at_eof: false,
    };
    let staged = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit(
            "a.txt",
            text_locator(false, Some(3)),
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect("hinted match plans");
    assert_eq!(&*staged.after.expect("after"), b"x\ny\nZ\n");
    let error = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit(
            "a.txt",
            text_locator(false, None),
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect_err("unhinted duplicate rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    let error = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit(
            "a.txt",
            text_locator(false, Some(2)),
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect_err("hint on a non-match line rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    let staged = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit(
            "a.txt",
            text_locator(true, None),
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect("replace-all plans");
    assert_eq!(&*staged.after.expect("after"), b"Z\ny\nZ\n");
    let error = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "q".to_owned(),
                line_hint: None,
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect_err("absent needle rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
}

#[tokio::test]
async fn stage_text_all_seen_gate_and_overlap_scan() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let all = |needle: &str| Locator::Text {
        old: needle.to_owned(),
        line_hint: None,
        all: true,
        window: Window::BeforePayload,
        context: None,
        at_eof: false,
    };
    // Seen guard without coverage: replace-all fails proof.
    let error = stage_one(
        &session,
        path,
        b"x\ny\nx\n",
        vec![change_edit_guard(
            "a.txt",
            all("x"),
            Action::Replace,
            Guard::Seen,
            "Z",
        )],
    )
    .await
    .expect_err("all+seen without coverage rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    // With full coverage the same edit plans.
    let bytes = b"x\ny\nx\n";
    let digest = *blake3::hash(bytes).as_bytes();
    session.seen.show(session.session, "a.txt", digest, 1, 3);
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            all("x"),
            Action::Replace,
            Guard::Seen,
            "Z",
        )],
    )
    .await
    .expect("covered all plans");
    assert_eq!(&*staged.after.expect("after"), b"Z\ny\nZ\n");
    // Overlapping needle: "aa" in "aaa" matches twice under the one-step
    // scan, and the descending splice collapses both spans into one write.
    let staged = stage_one(
        &session,
        path,
        b"h\naaa\nt\n",
        vec![change_edit_guard(
            "a.txt",
            all("aa"),
            Action::Replace,
            Guard::Quoted,
            "Z",
        )],
    )
    .await
    .expect("overlapping all plans");
    assert_eq!(&*staged.after.expect("after"), b"h\nZ\nt\n");
}

#[tokio::test]
async fn stage_span_requires_seen_coverage() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let span = |first, last| Locator::Span {
        first,
        last,
        quoted: vec![],
    };
    let error = stage_one(
        &session,
        path,
        b"a\nb\nc\n",
        vec![change_edit("a.txt", span(1, 2), Action::Replace, "X\n")],
    )
    .await
    .expect_err("unseen span rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    let bytes = b"a\nb\nc\n";
    let digest = *blake3::hash(bytes).as_bytes();
    session.seen.show(session.session, "a.txt", digest, 1, 2);
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit("a.txt", span(1, 2), Action::Replace, "X\n")],
    )
    .await
    .expect("covered span plans");
    assert_eq!(&*staged.after.expect("after"), b"X\nc\n");
    let error = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit("a.txt", span(1, 9), Action::Replace, "X\n")],
    )
    .await
    .expect_err("past-end span rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
}

#[tokio::test]
async fn stage_whole_and_tag_guards() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let bytes = b"a\nb\n";
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Whole,
            Action::Replace,
            "new\n",
        )],
    )
    .await
    .expect("whole plans");
    assert_eq!(&*staged.after.expect("after"), b"new\n");
    // Seen + Whole without coverage fails proof; covered passes.
    let error = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            Locator::Whole,
            Action::Replace,
            Guard::Seen,
            "new\n",
        )],
    )
    .await
    .expect_err("seen whole without coverage rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    let digest = *blake3::hash(bytes).as_bytes();
    session.seen.show(session.session, "a.txt", digest, 1, 2);
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            Locator::Whole,
            Action::Replace,
            Guard::Seen,
            "new\n",
        )],
    )
    .await
    .expect("covered whole plans");
    assert_eq!(&*staged.after.expect("after"), b"new\n");
    // Tag guards: wrong tags are Stale, the right tag plans.
    for guard in [
        Guard::Version("ffff".to_owned()),
        Guard::WholeTag("deadbeef".to_owned()),
    ] {
        let error = stage_one(
            &session,
            path,
            bytes,
            vec![change_edit_guard(
                "a.txt",
                Locator::Lines { first: 1, last: 1 },
                Action::Replace,
                guard,
                "X\n",
            )],
        )
        .await
        .expect_err("stale tag rejects");
        assert_eq!(error.class, super::ir::ErrorClass::Stale);
    }
    let version = format!("{:.4}", crate::tag8("version", bytes));
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            Guard::Version(version),
            "X\n",
        )],
    )
    .await
    .expect("fresh tag plans");
    assert_eq!(&*staged.after.expect("after"), b"X\nb\n");
}

#[tokio::test]
async fn stage_crlf_and_bom_map_back_to_raw_bytes() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    // BOM + CRLF: the view strips both; staged bytes must stay faithful.
    let bytes = b"\xef\xbb\xbfa\r\nb\r\nc\r\n";
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Lines { first: 2, last: 2 },
            Action::Replace,
            "X\n",
        )],
    )
    .await
    .expect("crlf lines plan");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nX\r\nc\r\n"
    );
    // InsertAfter must map the end offset through removed CRs and the BOM.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Lines { first: 1, last: 1 },
            Action::InsertAfter,
            "Y\n",
        )],
    )
    .await
    .expect("crlf insert plans");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nY\r\nb\r\nc\r\n"
    );
}

#[tokio::test]
async fn stage_orders_descending_edits_once_each() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let staged = stage_one(
        &session,
        path,
        b"a\nb\nc\n",
        vec![
            change_edit(
                "a.txt",
                Locator::Lines { first: 1, last: 1 },
                Action::Replace,
                "P\n",
            ),
            Edit::Change {
                index: 1,
                path: std::path::PathBuf::from("a.txt"),
                locator: Locator::Lines { first: 3, last: 3 },
                action: Action::Replace,
                guard: Guard::Quoted,
                body: "Q\n".to_owned(),
                window: Window::BeforePayload,
            },
        ],
    )
    .await
    .expect("multi edit plans");
    assert_eq!(&*staged.after.expect("after"), b"P\nb\nQ\n");
}

#[tokio::test]
async fn stage_classify_create_delete_rename() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    // Create on an absent path stages Create with no before image.
    let staged = stage::stage_file(
        &session,
        DialectId::Replace,
        std::path::Path::new("new.txt"),
        &dir.path().join("new.txt"),
        vec![Edit::Create {
            index: 0,
            path: std::path::PathBuf::from("new.txt"),
            body: "fresh\n".to_owned(),
        }],
    )
    .await
    .expect("create plans");
    assert_eq!(staged.op, Operation::Create);
    assert!(staged.before.is_none());
    assert_eq!(&*staged.after.expect("after"), b"fresh\n");
    // Create over an existing file is a File error.
    let path = std::path::Path::new("a.txt");
    let error = stage_one(
        &session,
        path,
        b"here\n",
        vec![Edit::Create {
            index: 0,
            path: std::path::PathBuf::from("a.txt"),
            body: "x".to_owned(),
        }],
    )
    .await
    .expect_err("existing create rejects");
    assert_eq!(error.class, super::ir::ErrorClass::File);
    // Delete stages after=None.
    let staged = stage_one(
        &session,
        path,
        b"here\n",
        vec![Edit::Delete {
            index: 0,
            path: std::path::PathBuf::from("a.txt"),
            reference: None,
        }],
    )
    .await
    .expect("delete plans");
    assert_eq!(staged.op, Operation::Delete);
    assert!(staged.after.is_none());
    // Rename plus a change stages op=Rename with the destination.
    let staged = stage_one(
        &session,
        path,
        b"a\nb\n",
        vec![
            change_edit(
                "a.txt",
                Locator::Lines { first: 1, last: 1 },
                Action::Replace,
                "P\n",
            ),
            Edit::Rename {
                index: 1,
                from: std::path::PathBuf::from("a.txt"),
                to: std::path::PathBuf::from("b.txt"),
                reference: None,
            },
        ],
    )
    .await
    .expect("rename plans");
    assert_eq!(staged.op, Operation::Rename);
    let dest = staged.renamed_to.as_ref().expect("rename destination");
    assert_eq!(dest.path, std::path::Path::new("b.txt"));
    assert_eq!(
        dest.absolute_path,
        std::fs::canonicalize(dir.path())
            .expect("workspace canonicalizes")
            .join("b.txt")
    );
    assert_eq!(&*staged.after.expect("after"), b"P\nb\n");
    // Two renames in one payload conflict.
    let error = stage_one(
        &session,
        path,
        b"a\n",
        vec![
            Edit::Rename {
                index: 0,
                from: std::path::PathBuf::from("a.txt"),
                to: std::path::PathBuf::from("b.txt"),
                reference: None,
            },
            Edit::Rename {
                index: 1,
                from: std::path::PathBuf::from("a.txt"),
                to: std::path::PathBuf::from("c.txt"),
                reference: None,
            },
        ],
    )
    .await
    .expect_err("double rename rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "std has no mkfifo and rustix compiles mkfifoat out on Apple targets"
)]
async fn patch_rejects_fifo_before_reading() {
    use std::{process::Command, time::Duration};

    let dir = tempfile::tempdir().expect("temp workspace");
    let fifo = dir.path().join("pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("create fifo");
    assert!(status.success(), "mkfifo failed with {status}");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("pipe");
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        stage::stage_file(
            &session,
            DialectId::Replace,
            path,
            &fifo,
            vec![change_edit(
                "pipe",
                Locator::Whole,
                Action::Replace,
                "replacement\n",
            )],
        ),
    )
    .await
    .expect("FIFO patch must return without blocking");
    let error = result.expect_err("FIFO patch must be rejected");
    assert_eq!(error.class, super::ir::ErrorClass::File);
    assert!(error.message.contains("pipe"), "{}", error.message);
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "std has no mkfifo and rustix compiles mkfifoat out on Apple targets"
)]
async fn patch_rejects_fifo_reference_before_reading() {
    use std::{process::Command, time::Duration};

    let dir = tempfile::tempdir().expect("temp workspace");
    let fifo = dir.path().join("pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("create fifo");
    assert!(status.success(), "mkfifo failed with {status}");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("pipe");
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            path,
            b"source\n",
        )
        .expect("capture FIFO reference");
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        stage::stage_file(
            &session,
            DialectId::HashlineLight,
            path,
            &fifo,
            vec![Edit::Delete {
                index: 0,
                path: path.to_path_buf(),
                reference: Some(reference.display().to_string()),
            }],
        ),
    )
    .await
    .expect("FIFO reference patch must return without blocking");
    let error = result.expect_err("FIFO reference patch must be rejected");
    assert_eq!(error.class, super::ir::ErrorClass::File);
    assert!(error.message.contains("pipe"), "{}", error.message);
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "std has no mkfifo and rustix compiles mkfifoat out on Apple targets"
)]
async fn commit_rejects_fifo_after_staging() {
    use std::{process::Command, time::Duration};

    let dir = tempfile::tempdir().expect("temp workspace");
    let path = dir.path().join("pipe");
    tokio::fs::write(&path, b"before\n")
        .await
        .expect("seed regular file");
    let session = test_session(dir.path(), false);
    let staged = plan(
        &session,
        DialectId::Replace,
        "{\"changes\":[{\"path\":\"pipe\",\"old\":\"before\",\"new\":\"after\"}]}",
    )
    .await
    .expect("stage regular file patch");

    tokio::fs::remove_file(&path)
        .await
        .expect("remove regular file");
    let status = Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("create fifo");
    assert!(status.success(), "mkfifo failed with {status}");

    let output = tokio::time::timeout(Duration::from_secs(1), commit(&session, staged, &[]))
        .await
        .expect("FIFO commit must return without blocking");
    assert_eq!(output.error_class, Some(super::ir::ErrorClass::File));
    assert!(output.text.contains("pipe"), "{}", output.text);
}

#[cfg(unix)]
#[tokio::test]
async fn commit_restores_earlier_deletes_when_a_later_delete_fails() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("temp workspace");
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).expect("locked dir");
    std::fs::write(dir.path().join("first.txt"), b"first\n").expect("seed first");
    std::fs::write(locked.join("second.txt"), b"second\n").expect("seed second");
    let session = test_session(dir.path(), false);
    let mut staged = plan(
        &session,
        DialectId::ApplyPatch,
        "*** Begin Patch\n*** Delete File: first.txt\n*** Delete File: locked/second.txt\n*** End Patch\n",
    )
    .await
    .expect("stage two deletes");
    // The read-only directory must be the delete that fails: commit visits
    // files in plan order, and lexical order already puts first.txt first.
    staged
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    let mut permissions = std::fs::metadata(&locked)
        .expect("locked metadata")
        .permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&locked, permissions).expect("lock the directory");

    let output = commit(&session, staged, &[]).await;

    let mut permissions = std::fs::metadata(&locked)
        .expect("locked metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&locked, permissions).expect("unlock the directory");
    assert_eq!(output.error_class, Some(super::ir::ErrorClass::Io));
    assert!(
        output.text.contains("Nothing was written"),
        "{}",
        output.text
    );
    assert_eq!(
        std::fs::read(dir.path().join("first.txt")).expect("first delete restored"),
        b"first\n"
    );
    assert_eq!(
        std::fs::read(locked.join("second.txt")).expect("second file intact"),
        b"second\n"
    );
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read workspace")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().contains(".dalgon-trash"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "trash files left behind: {leftovers:?}"
    );
}

#[tokio::test]
async fn stage_replacement_contract() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let canonical = dir.path().join("a.txt");
    let display = std::path::Path::new("a.txt");
    tokio::fs::write(&canonical, b"one\ntwo\nthree\n")
        .await
        .expect("seed");
    let staged = stage::stage_replacement(display, &canonical, b"two", b"dos", 2)
        .await
        .expect("replacement plans");
    assert_eq!(&*staged.after.expect("after"), b"one\ndos\nthree\n");
    assert_eq!(staged.op, Operation::Update);
    // A match that starts inside the line, not at its first byte.
    let staged = stage::stage_replacement(display, &canonical, b"wo", b"dos", 2)
        .await
        .expect("mid-line match plans");
    assert_eq!(&*staged.after.expect("after"), b"one\ntdos\nthree\n");
    for (before, line) in [
        (&b"two"[..], 0_u32),
        (&b""[..], 1),
        (&b"four"[..], 2),
        (&b"two"[..], 9),
        // Real bytes but only reachable past the target line's end or
        // outside the scan bound: each must stay a miss.
        (&b"hr"[..], 2),
        // A newline-leading needle only matches under a widened scan bound.
        (&b"\nx"[..], 3),
        // First byte matches at several positions; full needle never does.
        (&b"tx"[..], 2),
    ] {
        let error = stage::stage_replacement(display, &canonical, before, b"x", line)
            .await
            .expect_err("bad replacement rejects");
        assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    }
    // Without a trailing newline, line 4 does not exist but the scan cursor
    // still sits on "three": the line-exists guard must reject anyway.
    let canonical_b = dir.path().join("b.txt");
    let display_b = std::path::Path::new("b.txt");
    tokio::fs::write(&canonical_b, b"one\ntwo\nthree")
        .await
        .expect("seed");
    let error = stage::stage_replacement(display_b, &canonical_b, b"three", b"x", 4)
        .await
        .expect_err("missing line rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    let staged = stage::stage_replacement(display_b, &canonical_b, b"three", b"x", 3)
        .await
        .expect("unterminated last line still matches");
    assert_eq!(&*staged.after.expect("after"), b"one\ntwo\nx");
}

#[tokio::test]
async fn stage_non_replace_dialect_emits_no_hunks() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let canonical = session.workspace.join(path);
    tokio::fs::write(&canonical, b"a\nb\n").await.expect("seed");
    let staged = stage::stage_file(
        &session,
        DialectId::Anchor,
        path,
        &canonical,
        vec![change_edit(
            "a.txt",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            "P\n",
        )],
    )
    .await
    .expect("anchor stages");
    assert!(staged.hunks.is_empty(), "non-replace dialects defer hunks");
}

#[tokio::test]
async fn stage_boundary_lines_gap_span() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    // Lines reaching the last line: end resolves to view.len().
    let staged = stage_one(
        &session,
        path,
        b"a\nb\nc\nd\n",
        vec![change_edit(
            "a.txt",
            Locator::Lines { first: 1, last: 4 },
            Action::Replace,
            "Z\n",
        )],
    )
    .await
    .expect("full-range lines plan");
    assert_eq!(&*staged.after.expect("after"), b"Z\n");
    // Gap exactly one past the final line plans (append position).
    let staged = stage_one(
        &session,
        path,
        b"a\nb\n",
        vec![change_edit(
            "a.txt",
            Locator::Gap { before_line: 3 },
            Action::InsertAfter,
            "X\n",
        )],
    )
    .await
    .expect("past-end gap plans");
    assert_eq!(&*staged.after.expect("after"), b"a\nb\nX\n");
    // Boundary spans: first == last and last == count both plan.
    let bytes = b"a\nb\nc\n";
    let digest = *blake3::hash(bytes).as_bytes();
    session.seen.show(session.session, "a.txt", digest, 1, 3);
    for (first, last, expected) in [
        (1_usize, 1_usize, &b"X\nb\nc\n"[..]),
        (1, 2, b"X\nc\n"),
        (2, 3, b"a\nX\n"),
        (3, 3, b"a\nb\nX\n"),
        (1, 3, b"X\n"),
    ] {
        let staged = stage_one(
            &session,
            path,
            bytes,
            vec![change_edit(
                "a.txt",
                Locator::Span {
                    first,
                    last,
                    quoted: vec![],
                },
                Action::Replace,
                "X\n",
            )],
        )
        .await
        .expect("boundary span plans");
        assert_eq!(
            &*staged.after.expect("after"),
            expected,
            "span {first}-{last}"
        );
    }
    // Invalid span combinations reject before any coverage check.
    for (first, last) in [(0_usize, 2_usize), (2, 1)] {
        let error = stage_one(
            &session,
            path,
            bytes,
            vec![change_edit(
                "a.txt",
                Locator::Span {
                    first,
                    last,
                    quoted: vec![],
                },
                Action::Replace,
                "X\n",
            )],
        )
        .await
        .expect_err("invalid span rejects");
        assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    }
}

#[expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
#[tokio::test]
async fn stage_text_maps_through_bom_and_crlf() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let bytes = b"\xef\xbb\xbfa\r\nb\r\nc\r\n";
    // The view strips BOM and CR; the staged span must land on raw bytes.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "b\n".to_owned(),
                line_hint: None,
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::Replace,
            "Z\n",
        )],
    )
    .await
    .expect("bom text plans");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nZ\r\nc\r\n"
    );
    // InsertBefore maps the start offset the same way.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "b".to_owned(),
                line_hint: None,
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::InsertBefore,
            "Y\n",
        )],
    )
    .await
    .expect("bom insert plans");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nY\r\nb\r\nc\r\n"
    );
    // A match ending exactly on a removed CR's view offset maps back
    // without swallowing the CR (offset-boundary regression).
    let staged = stage_one(
        &session,
        path,
        b"a\r\nb\r\n",
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "a".to_owned(),
                line_hint: None,
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::Replace,
            "X",
        )],
    )
    .await
    .expect("cr-boundary plans");
    assert_eq!(&*staged.after.expect("after"), b"X\r\nb\r\n");
    // A CRLF-carrying needle normalizes before matching: it must match the
    // stripped view and still land on raw bytes with CRs preserved.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "b\r\n".to_owned(),
                line_hint: None,
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::Replace,
            "Z\n",
        )],
    )
    .await
    .expect("crlf needle plans");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nZ\r\nc\r\n"
    );
    // A hinted match still maps to raw bytes, not view offsets.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit(
            "a.txt",
            Locator::Text {
                old: "b".to_owned(),
                line_hint: Some(2),
                all: false,
                window: Window::BeforePayload,
                context: None,
                at_eof: false,
            },
            Action::Replace,
            "Z",
        )],
    )
    .await
    .expect("bom hinted plans");
    assert_eq!(
        &*staged.after.expect("after"),
        b"\xef\xbb\xbfa\r\nZ\r\nc\r\n"
    );
}

#[tokio::test]
async fn stage_seen_whole_empty_file_skips_coverage() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    // An empty file has zero lines; the Whole+Seen coverage gate opens.
    let staged = stage_one(
        &session,
        path,
        b"",
        vec![change_edit_guard(
            "a.txt",
            Locator::Whole,
            Action::Replace,
            Guard::Seen,
            "new\n",
        )],
    )
    .await
    .expect("empty whole plans");
    assert_eq!(&*staged.after.expect("after"), b"new\n");
}

#[tokio::test]
async fn stage_stale_version_names_current_tag() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let bytes = b"a\nb\n";
    let version = format!("{:.4}", crate::tag8("version", bytes));
    let whole = crate::tag8("whole", bytes);
    let error = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            Guard::Version("ffff".to_owned()),
            "X\n",
        )],
    )
    .await
    .expect_err("stale version rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Stale);
    assert!(
        error.message.contains(&version),
        "stale error names the current version tag: {}",
        error.message
    );
    assert!(!error.message.contains(&whole), "version error, not whole");
    // The whole tag path also plans on a fresh tag.
    let staged = stage_one(
        &session,
        path,
        bytes,
        vec![change_edit_guard(
            "a.txt",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            Guard::WholeTag(whole),
            "X\n",
        )],
    )
    .await
    .expect("fresh whole tag plans");
    assert_eq!(&*staged.after.expect("after"), b"X\nb\n");
}

#[tokio::test]
async fn stage_node_symbol_reject_when_symbols_disabled() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.rs");
    for locator in [
        Locator::Node { first_line: 1 },
        Locator::Symbol {
            name: "one".to_owned(),
            ordinal: None,
            old: None,
        },
    ] {
        let error = stage_one(
            &session,
            path,
            b"fn one() {}\n",
            vec![change_edit("a.rs", locator, Action::Replace, "x\n")],
        )
        .await
        .expect_err("disabled symbols reject");
        assert_eq!(error.class, super::ir::ErrorClass::Resolve);
        assert!(
            error.message.contains("symbol support is not enabled"),
            "{}",
            error.message
        );
    }
    // A DefTag guard on any locator rejects on a symbols-disabled
    // session before it can resolve a definition.
    let error = stage_one(
        &session,
        path,
        b"fn one() {}\n",
        vec![change_edit_guard(
            "a.rs",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            Guard::DefTag("any".to_owned()),
            "x\n",
        )],
    )
    .await
    .expect_err("deftag on disabled session rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    assert!(
        error.message.contains("symbol support is not enabled"),
        "{}",
        error.message
    );
}

#[cfg(feature = "symbols")]
#[expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
#[tokio::test]
async fn stage_node_locator_coverage_and_language() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), true);
    let rs = std::path::Path::new("a.rs");
    let bytes = b"fn one() {\n    let x = 1;\n}\n\nfn two() {\n    let y = 2;\n}\n";
    // Replace: the outermost node at line 1 is `fn one`, lines 1-3.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            Locator::Node { first_line: 1 },
            Action::Replace,
            "fn three() {}\n",
        )],
    )
    .await
    .expect("node plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(after, "fn three() {}\n\nfn two() {\n    let y = 2;\n}\n");
    // Seen guard needs the node's line footprint covered.
    let error = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            Locator::Node { first_line: 1 },
            Action::Replace,
            Guard::Seen,
            "fn three() {}\n",
        )],
    )
    .await
    .expect_err("uncovered node rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    let digest = *blake3::hash(bytes).as_bytes();
    session.seen.show(session.session, "a.rs", digest, 1, 3);
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            Locator::Node { first_line: 1 },
            Action::Replace,
            Guard::Seen,
            "fn three() {}\n",
        )],
    )
    .await
    .expect("covered node plans");
    assert!(staged.after.is_some());
    // InsertAfter lands after the node's line span, not inside it.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            Locator::Node { first_line: 1 },
            Action::InsertAfter,
            "fn ins() {}\n",
        )],
    )
    .await
    .expect("node insert plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(
        after,
        "fn one() {\n    let x = 1;\n}\nfn ins() {}\n\nfn two() {\n    let y = 2;\n}\n"
    );
    // A non-symbol file rejects block ops outright.
    let error = stage_one(
        &session,
        std::path::Path::new("a.txt"),
        b"plain\n",
        vec![change_edit(
            "a.txt",
            Locator::Node { first_line: 1 },
            Action::Replace,
            "x\n",
        )],
    )
    .await
    .expect_err("txt node rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    // A valid Reference under a non-Enhanced dialect clears the proof
    // check but skips the coverage gate in node_coverage.
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            rs,
            bytes,
        )
        .expect("capture");
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            Locator::Node { first_line: 1 },
            Action::Replace,
            Guard::Reference(reference.display().to_string()),
            "fn three() {}\n",
        )],
    )
    .await
    .expect("reference under replace plans");
    assert!(staged.after.is_some());
}

#[cfg(feature = "symbols")]
#[tokio::test]
async fn stage_node_enhanced_reference_gate() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), true);
    let rs = std::path::Path::new("a.rs");
    let canonical = session.workspace.join(rs);
    let bytes = b"fn one() {\n    let x = 1;\n}\n\nfn two() {\n    let y = 2;\n}\n";
    let (reference, _) = session
        .snapshots
        .capture(
            session.session,
            session.generation,
            session.consumer,
            rs,
            bytes,
        )
        .expect("capture");
    let node_edit = || Edit::Change {
        index: 0,
        path: std::path::PathBuf::from("a.rs"),
        locator: Locator::Node { first_line: 1 },
        action: Action::Replace,
        guard: Guard::Reference(reference.display().to_string()),
        body: "fn three() {}\n".to_owned(),
        window: Window::BeforePayload,
    };
    tokio::fs::write(&canonical, bytes).await.expect("seed");
    // No delivery yet: the node footprint is unobserved. Enhanced dialect
    // is the only path that consults the snapshot ledger.
    let error = stage::stage_file(
        &session,
        DialectId::HashlineEnhanced,
        rs,
        &session.workspace.join(rs),
        vec![node_edit()],
    )
    .await
    .expect_err("undelivered enhanced node rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Proof);
    // Show then deliver the node's lines at a cutoff below the frozen
    // call cutoff.
    session.snapshots.show(reference, session.consumer, 1, 3);
    session
        .snapshots
        .deliver(session.session, session.consumer, reference, 1, 3, 7);
    let staged = stage::stage_file(
        &session,
        DialectId::HashlineEnhanced,
        rs,
        &session.workspace.join(rs),
        vec![node_edit()],
    )
    .await
    .expect("delivered enhanced node plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(after, "fn three() {}\n\nfn two() {\n    let y = 2;\n}\n");
}

#[cfg(feature = "symbols")]
#[expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
#[tokio::test]
async fn stage_symbol_locator_and_needle_narrowing() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), true);
    let rs = std::path::Path::new("a.rs");
    let bytes = b"fn one() {\n    let x = 1;\n}\n\nfn two() {\n    let y = 2;\n    let z = 3;\n}\n";
    let symbol = |old: Option<&str>| Locator::Symbol {
        name: "two".to_owned(),
        ordinal: None,
        old: old.map(str::to_owned),
    };
    // No `old`: the whole definition span is replaced.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            symbol(None),
            Action::Replace,
            "fn duo() {}\n",
        )],
    )
    .await
    .expect("symbol plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(after, "fn one() {\n    let x = 1;\n}\n\nfn duo() {}\n\n");
    // Unique `old` narrows the span to the matched fragment.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            symbol(Some("let y = 2")),
            Action::Replace,
            "let y = 9",
        )],
    )
    .await
    .expect("needle plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(
        after,
        "fn one() {\n    let x = 1;\n}\n\nfn two() {\n    let y = 9;\n    let z = 3;\n}\n"
    );
    // A needle spanning the whole definition is still a unique match.
    let whole_def = "fn two() {\n    let y = 2;\n    let z = 3;\n}";
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            symbol(Some(whole_def)),
            Action::Replace,
            "fn duo() {}",
        )],
    )
    .await
    .expect("span-sized needle plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(after, "fn one() {\n    let x = 1;\n}\n\nfn duo() {}\n");
    // InsertBefore lands ahead of the definition's first byte.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit(
            "a.rs",
            symbol(None),
            Action::InsertBefore,
            "// doc\n",
        )],
    )
    .await
    .expect("symbol insert plans");
    let after = String::from_utf8(staged.after.expect("after").into_vec()).expect("utf8");
    assert_eq!(
        after,
        "fn one() {\n    let x = 1;\n}\n\n// doc\nfn two() {\n    let y = 2;\n    let z = 3;\n}\n"
    );
    // Ambiguous and absent needles reject.
    for needle in ["let", "not-present"] {
        let error = stage_one(
            &session,
            rs,
            bytes,
            vec![change_edit(
                "a.rs",
                symbol(Some(needle)),
                Action::Replace,
                "X",
            )],
        )
        .await
        .expect_err("bad needle rejects");
        assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    }
    // DefTag on a Symbol locator: empty or current tag plans, wrong tag
    // is Stale.
    let (_, _, current_tag) =
        super::ast::symbol_span(&session.workspace.join(rs), bytes, "two", None)
            .await
            .expect("tag resolves");
    for expected in [String::new(), current_tag.clone()] {
        let staged = stage_one(
            &session,
            rs,
            bytes,
            vec![change_edit_guard(
                "a.rs",
                symbol(None),
                Action::Replace,
                Guard::DefTag(expected),
                "fn duo() {}\n",
            )],
        )
        .await
        .expect("matching tag plans");
        assert!(staged.after.is_some());
    }
    let error = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            symbol(None),
            Action::Replace,
            Guard::DefTag("bogus-tag".to_owned()),
            "fn duo() {}\n",
        )],
    )
    .await
    .expect_err("stale tag rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Stale);
    // Unknown definitions reject; unknown names list no candidates.
    let unknown = Locator::Symbol {
        name: "nope".to_owned(),
        ordinal: None,
        old: None,
    };
    let error = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit("a.rs", unknown, Action::Replace, "X")],
    )
    .await
    .expect_err("unknown definition rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
    // DefTag: empty or current tag plans; a wrong tag is Stale.
    let staged = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            symbol(None),
            Action::Replace,
            Guard::DefTag(String::new()),
            "fn duo() {}\n",
        )],
    )
    .await
    .expect("empty def tag plans");
    assert!(staged.after.is_some());
    let error = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            symbol(None),
            Action::Replace,
            Guard::DefTag("wrongtag".to_owned()),
            "fn duo() {}\n",
        )],
    )
    .await
    .expect_err("stale def tag rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Stale);
    // DefTag on a non-symbol locator resolves, not panics.
    let error = stage_one(
        &session,
        rs,
        bytes,
        vec![change_edit_guard(
            "a.rs",
            Locator::Lines { first: 1, last: 1 },
            Action::Replace,
            Guard::DefTag(String::new()),
            "X\n",
        )],
    )
    .await
    .expect_err("def tag on lines rejects");
    assert_eq!(error.class, super::ir::ErrorClass::Resolve);
}
#[tokio::test]
async fn text_locator_accepts_canonical_equivalence() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let session = test_session(dir.path(), false);
    let path = std::path::Path::new("a.txt");
    let locator = Locator::Text {
        old: "let value = 'ok'".to_owned(),
        line_hint: None,
        all: false,
        window: Window::BeforePayload,
        context: None,
        at_eof: false,
    };
    let staged = stage_one(
        &session,
        path,
        "let value = ‘ok’  \r\n".as_bytes(),
        vec![change_edit(
            "a.txt",
            locator,
            Action::Replace,
            "let value = 'new'",
        )],
    )
    .await
    .expect("canonical text plans");
    assert_eq!(
        &*staged.after.expect("after"),
        "let value = 'new'\r\n".as_bytes()
    );
}

#[test]
fn text_locator_large_file_has_linear_work() {
    let haystack = format!("{}c", "a".repeat(512 * 1024));
    let needle = format!("{}b", "a".repeat(2048));
    let comparisons = super::write::stage::find_text_matches_linear_probe(&haystack, &needle);
    assert!(
        comparisons <= haystack.len().saturating_add(needle.len()) * 4,
        "matcher comparisons grew superlinearly: {comparisons}"
    );
}

fn envelope_tool(style: &str) -> Arc<dyn Tool> {
    super::tool(
        &EditStyleInput::Scalar(style.into()),
        crate::search::index::Index::new(None),
        crate::Seen::new(),
        Arc::new(SnapshotStore::new([7; 16])),
        None,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .expect("patch tool registers")
}

fn classify_probe_name(dialect: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "classify-probe-{dialect}-{}-{}.txt",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[tokio::test]
async fn patch_dispatch_envelope_applies_for_every_freeform_dialect() {
    // Dispatch classifies before running, and the freeform dialects register
    // the `{"input": string}` envelope: a call shaped that way must classify
    // and then apply, whatever the configured edit style is.
    let cases: [(&str, &str, &str); 5] = [
        ("hashline", "[{name}#NEW]\nPUT >$:\n+body\n", "body\n"),
        ("hashline-light", "[{name}#NEW]\nPUT >$:\n+body\n", "body\n"),
        (
            "hashline-enhanced",
            "[{name}#NEW]\nPUT >$:\n+body\n",
            "body\n",
        ),
        ("anchor", "*** New File: {name}\nbody\n", "body\n"),
        (
            "apply_patch",
            "*** Begin Patch\n*** Add File: {name}\n+body\n*** End Patch\n",
            "body\n",
        ),
    ];
    let mut rejections: Vec<String> = Vec::new();
    for (style, template, expected) in cases {
        let tool = envelope_tool(style);
        let name = classify_probe_name(style);
        let payload = template.replace("{name}", &name);
        let args = format!(
            "{{\"input\":{}}}",
            sonic_rs::to_string(&payload).expect("payload encodes")
        );
        let cx = ToolCx::for_test_approved(Arc::new(NeverServices));
        let raw = RawValue::parse(&args).expect("envelope is one JSON value");
        if let Err(error) = tool.classify(&raw, cx.workspace()) {
            rejections.push(format!("{style}: {error}"));
            continue;
        }
        let outcome = tool.run(ToolCall::new("probe", raw), cx).await;
        let ToolOutcome::Ok(output) = outcome else {
            rejections.push(format!("{style}: call was denied"));
            continue;
        };
        let path = std::env::temp_dir().join(&name);
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(written, expected, "{style} applied: {output:?}");
        let _ = std::fs::remove_file(&path);
    }
    assert!(
        rejections.is_empty(),
        "the registered input envelope must classify for every freeform dialect: {rejections:?}"
    );
}

#[tokio::test]
async fn patch_dispatch_structured_changes_apply_for_replace() {
    // The structured dialect keeps its registered `{"changes": [...]}` shape
    // through the same classify-then-run order.
    let tool = envelope_tool("replace");
    let name = classify_probe_name("replace");
    let args = format!("{{\"changes\":[{{\"path\":\"{name}\",\"create\":\"body\\n\"}}]}}");
    let cx = ToolCx::for_test_approved(Arc::new(NeverServices));
    let raw = RawValue::parse(&args).expect("structured call is one JSON value");
    let class = tool
        .classify(&raw, cx.workspace())
        .expect("structured call classifies");
    assert_eq!(class, ToolClass::Patch);
    let outcome = tool.run(ToolCall::new("probe", raw), cx).await;
    let ToolOutcome::Ok(output) = outcome else {
        panic!("structured call must not be denied");
    };
    let path = std::env::temp_dir().join(&name);
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(written, "body\n", "replace applied: {output:?}");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn patch_dispatch_malformed_json_gets_documented_error() {
    // `{`-leading args that decode as neither registered envelope are
    // refused with the documented invalid-input text.
    let tool = envelope_tool("hashline");
    let cx = ToolCx::for_test_approved(Arc::new(NeverServices));
    for args in [
        "{\"input\": 5}",
        "{\"changes\": \"array\"}",
        "{\"nope\": 1}",
    ] {
        let raw = RawValue::parse(args).expect("probe text is one JSON value");
        let error = tool
            .classify(&raw, cx.workspace())
            .expect_err("args matching no envelope must be refused");
        assert!(
            error.to_string().starts_with("patch: invalid input:"),
            "documented error text, got: {error}"
        );
    }
}

#[cfg(unix)]
mod modes {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use crate::patch::ir::ErrorClass;

    use super::*;

    fn file_mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path)
            .expect("published file metadata")
            .mode()
            & 0o777
    }

    fn restrict(path: &std::path::Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))
            .expect("seed permissions");
    }

    #[tokio::test]
    async fn commit_update_preserves_target_mode() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let path = dir.path().join("secret.txt");
        tokio::fs::write(&path, b"before\n").await.expect("seed");
        restrict(&path, 0o600);
        let session = test_session(dir.path(), false);
        let input =
            "{\"changes\":[{\"path\":\"secret.txt\",\"old\":\"before\",\"new\":\"after\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"after\n");
        assert_eq!(file_mode(&path), 0o600, "update keeps the target mode");
    }

    #[tokio::test]
    async fn commit_update_preserves_executable_mode() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let path = dir.path().join("script.sh");
        tokio::fs::write(&path, b"old\n").await.expect("seed");
        restrict(&path, 0o755);
        let session = test_session(dir.path(), false);
        let input = "{\"changes\":[{\"path\":\"script.sh\",\"old\":\"old\",\"new\":\"new\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"new\n");
        assert_eq!(file_mode(&path), 0o755, "exec bit survives an update");
    }

    #[tokio::test]
    async fn commit_update_preserves_readonly_mode() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let path = dir.path().join("locked.txt");
        tokio::fs::write(&path, b"before\n").await.expect("seed");
        restrict(&path, 0o400);
        let session = test_session(dir.path(), false);
        let input =
            "{\"changes\":[{\"path\":\"locked.txt\",\"old\":\"before\",\"new\":\"after\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"after\n");
        assert_eq!(file_mode(&path), 0o400, "read-only target stays read-only");
    }

    #[tokio::test]
    async fn commit_update_stamps_bits_the_umask_narrowed_at_creation() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let path = dir.path().join("shared.txt");
        tokio::fs::write(&path, b"before\n").await.expect("seed");
        restrict(&path, 0o664);
        let session = test_session(dir.path(), false);
        let input =
            "{\"changes\":[{\"path\":\"shared.txt\",\"old\":\"before\",\"new\":\"after\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"after\n");
        // The staged temp is born with the saved mode narrowed by the umask,
        // which drops group write under the usual 022. Only the exact stamp
        // on the open handle restores it before publication.
        assert_eq!(file_mode(&path), 0o664, "update keeps narrowed bits");
    }

    #[tokio::test]
    async fn failed_later_publication_restores_a_published_read_only_update() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let locked = dir.path().join("locked.txt");
        tokio::fs::write(&locked, b"before\n").await.expect("seed");
        // The owner class denies writing, so the published file cannot be
        // reopened for write. The group bit is one a 022 umask strips at
        // creation, so only the exact stamp can bring it back.
        restrict(&locked, 0o464);
        // Planning and staging only touch short temp names, so a name past
        // NAME_MAX is refused only when publication renames onto it: a real
        // failure after the earlier update has already published.
        let overlong = "x".repeat(300);
        assert!(
            std::fs::File::create(dir.path().join(&overlong)).is_err(),
            "fixture needs a filesystem that refuses 300-byte names"
        );
        let session = test_session(dir.path(), false);
        let created = format!(r#"{{"path":"{overlong}","create":"body\n"}}"#);
        let input = format!(
            r#"{{"changes":[{{"path":"locked.txt","old":"before","new":"after"}},{created}]}}"#
        );
        let staged = plan(&session, DialectId::Replace, &input)
            .await
            .expect("plan");
        // Commit publishes in plan order, and plans sort by path.
        assert_eq!(staged.files[0].path, std::path::Path::new("locked.txt"));

        let output = commit(&session, staged, &[]).await;

        assert_eq!(output.error_class, Some(ErrorClass::Io));
        assert!(
            output.text.contains("Nothing was written"),
            "{}",
            output.text
        );
        assert_eq!(tokio::fs::read(&locked).await.expect("read"), b"before\n");
        assert_eq!(file_mode(&locked), 0o464, "rollback keeps the mode");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read workspace")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        assert_eq!(names, ["locked.txt"], "nothing else survives");
    }

    #[tokio::test]
    async fn commit_create_uses_0644_under_umask() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let path = dir.path().join("new.txt");
        let session = test_session(dir.path(), false);
        let input = "{\"changes\":[{\"path\":\"new.txt\",\"create\":\"body\\n\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"body\n");
        // 0644 can only be narrowed by the umask, never widened: group and
        // other write bits must be absent under any umask.
        let mode = file_mode(&path);
        assert_eq!(
            mode & 0o022,
            0,
            "new file must not be group- or other-writable: {mode:o}"
        );
    }

    #[tokio::test]
    async fn commit_rename_carries_source_mode_to_destination() {
        let dir = tempfile::tempdir().expect("temp workspace");
        std::fs::write(dir.path().join("a.txt"), b"alpha\n").expect("seed source");
        restrict(&dir.path().join("a.txt"), 0o750);
        let session = test_session(dir.path(), false);
        let input = "{\"changes\":[{\"path\":\"a.txt\",\"rename\":\"sub/b.txt\"}]}";
        let staged = plan(&session, DialectId::Replace, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        let destination = dir.path().join("sub/b.txt");
        assert_eq!(
            std::fs::read(&destination).expect("destination"),
            b"alpha\n"
        );
        assert_eq!(
            file_mode(&destination),
            0o750,
            "destination keeps the source mode"
        );
    }

    #[tokio::test]
    async fn commit_update_with_rename_preserves_source_mode() {
        let dir = tempfile::tempdir().expect("temp workspace");
        std::fs::write(dir.path().join("a.txt"), b"one\ntwo\n").expect("seed source");
        restrict(&dir.path().join("a.txt"), 0o750);
        let session = test_session(dir.path(), false);
        let input = concat!(
            "*** Begin Patch\n",
            "*** Update File: a.txt\n",
            "*** Move to: b.txt\n",
            "@@\n",
            "-two\n",
            "+dos\n",
            "*** End Patch\n",
        );
        let staged = plan(&session, DialectId::ApplyPatch, input)
            .await
            .expect("plan");
        let output = commit(&session, staged, &[]).await;
        assert!(output.error_class.is_none(), "{}", output.text);
        let destination = dir.path().join("b.txt");
        assert_eq!(
            std::fs::read(&destination).expect("destination"),
            b"one\ndos\n"
        );
        assert_eq!(file_mode(&destination), 0o750);
        assert!(std::fs::metadata(dir.path().join("a.txt")).is_err());
    }
}
