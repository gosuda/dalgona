//! The first-input hook against a recording `Services` seam.
//!
//! These tests pin the order and the failure points of the capture: every
//! blob lands before the first record, a cancellation or failure before the
//! records leaves the set `Empty`, a cancel never splits the record run, a
//! journal failure inside the run leaves a strict prefix that the next input
//! completes without a duplicate or conflicting id, and a well-formed row
//! outside the admitted bounds fails before any write, preserving the valid
//! rows and appending no duplicate id. The seam records what the
//! hook asks for; the real-host witnesses (real journal and blob store) live
//! in `tests/letter_first_prompt.rs`.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{Caller, HookCx, HookError, RawValue, Services, Tool, ToolOutcome};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse, PrivateEffect};
use dal_core::{
    AgentsOp, AgentsReply, Answer, BlobId, Caps, EntryId, FetchRequest, FetchResponse, Inference,
    InputEvent, InputVerdict, JobsOp, JobsReply, ModelInfo, ModelRequest, ModelRoute, Notice, Part,
    Question, RunOutput, RunRequest, SessionId, SidecarOp, StateError, StateOp, StateRecord,
    TurnOp, TurnOpReply, Visibility,
};
use dal_provider::EventStream;

use super::{
    Font, LetterAssembly, LetterChunk, LetterKind, SkillLetterRecord, handle_first_input, letters,
    matched_prefix,
};
use crate::skills::{RegisteredSkill, SkillRegistry};

/// One write the hook asked the seam for.
#[derive(Debug, PartialEq, Eq)]
enum Op {
    Blob,
    Record(Box<str>),
}

#[derive(Default)]
struct Tapped {
    writes: usize,
    ops: Vec<Op>,
    bodies: Vec<String>,
    notices: Vec<(String, String)>,
}

/// Records every blob and record write, serves canned `letter` rows and
/// delivery markers, and fails or cancels at a chosen write.
#[derive(Default)]
struct Tap {
    /// Letter rows `records("letter")` serves, as stored JSON.
    stored: Vec<String>,
    /// Delivery markers `records("letter_delivered")` serves.
    markers: Vec<String>,
    /// The 1-based write (blob or record) that fails without landing.
    fail_at: Option<usize>,
    /// The 1-based write after which the hook's cancel token fires.
    cancel_at: Option<usize>,
    cancel: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    log: Mutex<Tapped>,
}

impl Tap {
    fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn ops(&self) -> Vec<Op> {
        std::mem::take(&mut self.log.lock().expect("log").ops)
    }

    fn bodies(&self) -> Vec<String> {
        self.log.lock().expect("log").bodies.clone()
    }

    fn notices(&self) -> Vec<(String, String)> {
        self.log.lock().expect("log").notices.clone()
    }

    fn write(&self, op: Op, body: Option<String>) -> Result<(), ServiceError> {
        let mut log = self.log.lock().expect("log");
        log.writes += 1;
        let count = log.writes;
        if self.fail_at == Some(count) {
            return Err(ServiceError::failed(None, "disk full"));
        }
        log.ops.push(op);
        log.bodies.extend(body);
        drop(log);
        if self.cancel_at == Some(count)
            && let Some(cancel) = self.cancel.lock().expect("cancel").as_ref()
        {
            cancel();
        }
        Ok(())
    }
}

fn declined<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async { Err(ServiceError::Declined) })
}

impl Services for Tap {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        declined()
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        declined()
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        declined()
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        declined()
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        declined()
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        declined()
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        declined()
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        declined()
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        declined()
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        declined()
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        declined()
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        declined()
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        declined()
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        declined()
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        declined()
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        declined()
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        declined()
    }

    fn model_ready(
        &self,
        _who: &Caller,
        _model: ModelRoute,
    ) -> ServiceFuture<'_, Option<ModelRoute>> {
        declined()
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        declined()
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        declined()
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        declined()
    }

    fn notify(&self, _who: &Caller, notice: Notice) {
        self.log
            .lock()
            .expect("log")
            .notices
            .push((notice.kind.to_string(), notice.text.to_string()));
    }

    fn append_record(
        &self,
        _who: &Caller,
        kind: &str,
        body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        assert_eq!(kind, "letter", "the hook appends letter records only");
        let record: SkillLetterRecord = body.decode_as().expect("the hook writes strict rows");
        let outcome = self.write(Op::Record(record.id), Some(body.as_str().to_owned()));
        Box::pin(async move { outcome.map(|()| EntryId::new(NonZeroU64::MIN)) })
    }

    fn records(&self, _who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let rows = match kind {
            "letter" => &self.stored,
            "letter_delivered" => &self.markers,
            other => panic!("the hook reads no {other} records"),
        };
        let parsed: Vec<Box<RawValue>> = rows
            .iter()
            .map(|row| Box::new(RawValue::parse(row).expect("canned row parses")))
            .collect();
        Box::pin(async move { Ok(parsed) })
    }

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
        declined()
    }

    fn blob_put(&self, _who: &Caller, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        let digest = *BlobId::from_bytes(&bytes).as_bytes();
        let outcome = self.write(Op::Blob, None);
        Box::pin(async move { outcome.map(|()| digest) })
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        declined()
    }
}

fn registry(skills: &[(&str, &str)]) -> Arc<SkillRegistry> {
    let owned = skills
        .iter()
        .map(|(name, description)| RegisteredSkill {
            plugin: "p".into(),
            name: (*name).into(),
            description: (*description).into(),
            body: Arc::from("body"),
            letter2image: true,
            mcp: None,
        })
        .collect();
    Arc::new(SkillRegistry::merge(&[("p", owned)]).expect("no conflicts"))
}

fn drawn(registry: &SkillRegistry) -> LetterAssembly {
    letters(registry, &Font::embedded()).expect("the embedded font parses")
}

fn record_of(chunk: &LetterChunk) -> SkillLetterRecord {
    SkillLetterRecord {
        v: 1,
        id: chunk.id.clone(),
        kind: LetterKind::Skill,
        skill: chunk.skill.clone(),
        plugin: chunk.plugin.clone(),
        png_blob: BlobId::from_bytes(&chunk.png),
        source_blob: BlobId::from_bytes(chunk.source_text.as_bytes()),
        png_bytes: u64::try_from(chunk.png.len()).expect("png length fits"),
        width: chunk.width,
        height: chunk.height,
        cell: chunk.cell,
    }
}

/// The stored JSON row the hook would have written for `chunk`.
fn row(chunk: &LetterChunk) -> String {
    sonic_rs::to_string(&record_of(chunk)).expect("record serializes")
}

/// The delivery marker the session actor writes for `assembly`'s images.
fn marker(assembly: &LetterAssembly) -> String {
    let digests: Vec<String> = assembly
        .chunks
        .iter()
        .map(|chunk| format!("\"{}\"", BlobId::from_bytes(&chunk.png)))
        .collect();
    format!(r#"{{"v":1,"digests":[{}]}}"#, digests.join(","))
}

fn image_model() -> ModelInfo {
    ModelInfo {
        route: ModelRoute::from_id("p/model"),
        name: "p/model".into(),
        provider: "".into(),
        caps: Caps {
            context_window: None,
            thinking: Box::new([]),
            tool_use: false,
            image_input: true,
            custom_grammar: false,
        },
    }
}

/// Runs the real hook over `tap`, with the tap's cancel trigger wired to the
/// hook's own cancellation token.
async fn run(tap: &Arc<Tap>, registry: Arc<SkillRegistry>) -> Result<InputVerdict, HookError> {
    let session = SessionId::new_v7();
    let concrete: Arc<Tap> = Arc::clone(tap);
    let services: Arc<dyn Services> = concrete;
    let cx = HookCx::for_test(services, session, None);
    let token = cx.cancel.clone();
    *tap.cancel.lock().expect("cancel") = Some(Box::new(move || token.cancel()));
    let input = InputEvent {
        content: vec![Part::Text {
            text: "hello".into(),
        }],
        model: Some(image_model()),
    };
    handle_first_input(session, input, registry, Arc::new(Font::embedded()), cx).await
}

/// The letter images a transform carries ahead of the running text.
fn delivered_images(verdict: &InputVerdict) -> usize {
    let InputVerdict::Transform(parts) = verdict else {
        panic!("expected a transform, got {verdict:?}");
    };
    let images = parts
        .iter()
        .take_while(|part| matches!(part, Part::Image { mime, .. } if &**mime == "image/png"))
        .count();
    assert_eq!(
        &parts[images..],
        [Part::Text {
            text: "hello".into()
        }],
        "the running text follows the images unchanged"
    );
    images
}

fn blobs(count: usize) -> Vec<Op> {
    (0..count).map(|_| Op::Blob).collect()
}

fn record_op(id: &str) -> Op {
    Op::Record(id.into())
}

const TWO: [(&str, &str); 2] = [("alpha", "Review the diff."), ("beta", "Write the notes.")];

#[tokio::test]
async fn capture_writes_every_blob_before_the_first_record() {
    let tap = Tap::default().shared();
    let verdict = run(&tap, registry(&TWO)).await.expect("the hook succeeds");

    assert_eq!(delivered_images(&verdict), 2);
    let mut expected = blobs(4);
    expected.extend([record_op("1"), record_op("2")]);
    assert_eq!(tap.ops(), expected);
}

#[tokio::test]
async fn cancel_while_blobs_are_written_leaves_no_record() {
    let tap = Tap {
        cancel_at: Some(1),
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry(&TWO)).await;

    assert_eq!(result, Err(HookError::Cancelled));
    assert_eq!(
        tap.ops(),
        blobs(2),
        "only the first chunk's blobs landed; the set stays empty"
    );
}

#[tokio::test]
async fn cancel_after_the_last_blob_still_commits_no_record() {
    let tap = Tap {
        cancel_at: Some(4),
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry(&TWO)).await;

    assert_eq!(result, Err(HookError::Cancelled));
    assert_eq!(tap.ops(), blobs(4), "every blob landed, no record did");
}

#[tokio::test]
async fn cancel_inside_the_record_run_does_not_split_the_set() {
    let tap = Tap {
        cancel_at: Some(5),
        ..Tap::default()
    }
    .shared();
    let verdict = run(&tap, registry(&TWO)).await.expect("the run completes");

    assert_eq!(delivered_images(&verdict), 2);
    let mut expected = blobs(4);
    expected.extend([record_op("1"), record_op("2")]);
    assert_eq!(
        tap.ops(),
        expected,
        "a cancel during the record run finishes the run: no partial set"
    );
}

#[tokio::test]
async fn blob_failure_leaves_no_record_and_publishes_its_cause() {
    let tap = Tap {
        fail_at: Some(2),
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry(&TWO)).await;

    assert!(
        matches!(result, Err(HookError::Failed { .. })),
        "{result:?}"
    );
    assert_eq!(tap.ops(), blobs(1));
    assert_eq!(
        tap.notices(),
        [(
            "letter".to_owned(),
            "letter: skill letters were not persisted: disk full.".to_owned()
        )]
    );
}

#[tokio::test]
async fn record_failure_leaves_a_prefix_the_next_input_completes() {
    let registry = registry(&TWO);
    let tap = Tap {
        fail_at: Some(6),
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, Arc::clone(&registry)).await;

    assert!(
        matches!(result, Err(HookError::Failed { .. })),
        "{result:?}"
    );
    let mut expected = blobs(4);
    expected.push(record_op("1"));
    assert_eq!(
        tap.ops(),
        expected,
        "the second record failed after the first landed"
    );

    let resumed = Tap {
        stored: tap.bodies(),
        ..Tap::default()
    }
    .shared();
    let verdict = run(&resumed, registry).await.expect("the resume succeeds");

    assert_eq!(
        delivered_images(&verdict),
        2,
        "the completed set delivers every letter, the persisted one included"
    );
    assert_eq!(
        resumed.ops(),
        [Op::Blob, Op::Blob, record_op("2")],
        "only the missing tail is written, with the next id"
    );
}

#[tokio::test]
async fn a_full_undelivered_set_is_reattached_without_a_write() {
    let registry = registry(&TWO);
    let assembly = drawn(&registry);
    let tap = Tap {
        stored: assembly.chunks.iter().map(row).collect(),
        ..Tap::default()
    }
    .shared();
    let verdict = run(&tap, registry).await.expect("the hook succeeds");

    assert_eq!(delivered_images(&verdict), 2);
    assert_eq!(tap.ops(), []);
}

#[tokio::test]
async fn a_delivered_set_stays_frozen() {
    let registry = registry(&TWO);
    let assembly = drawn(&registry);
    let tap = Tap {
        stored: assembly.chunks.iter().map(row).collect(),
        markers: vec![marker(&assembly)],
        ..Tap::default()
    }
    .shared();
    let verdict = run(&tap, registry).await.expect("the hook succeeds");

    assert_eq!(verdict, InputVerdict::Continue);
    assert_eq!(tap.ops(), []);
}

#[tokio::test]
async fn a_changed_skill_set_never_appends_a_conflicting_id() {
    let stored_from = registry(&[("alpha", "Review the diff.")]);
    let stored = drawn(&stored_from);
    let tap = Tap {
        stored: stored.chunks.iter().map(row).collect(),
        ..Tap::default()
    }
    .shared();
    // `aardvark` now sorts first and takes id 1, so the stored `alpha` row no
    // longer matches position 1: the set freezes instead of growing.
    let changed = registry(&[
        ("aardvark", "Check the build."),
        ("alpha", "Review the diff."),
    ]);
    let verdict = run(&tap, changed).await.expect("the hook succeeds");

    assert_eq!(verdict, InputVerdict::Continue);
    assert_eq!(tap.ops(), []);
}

/// A well-formed row with an unsupported version fails before any write, so
/// the corrupt row can never hide behind a freshly persisted duplicate id.
#[tokio::test]
async fn well_formed_but_invalid_version_fails_before_any_write() {
    let registry = registry(&TWO);
    let assembly = drawn(&registry);
    let bad = row(&assembly.chunks[0]).replacen("\"v\":1", "\"v\":2", 1);
    let tap = Tap {
        stored: vec![bad],
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry).await;

    assert_eq!(
        result,
        Err(HookError::Failed {
            message: "letter record '1' failed validation".into(),
        }),
        "the invalid row fails closed with the resolver's validation text"
    );
    assert_eq!(tap.ops(), [], "no blob or record lands before the failure");
    assert_eq!(
        tap.notices(),
        [(
            "letter".to_owned(),
            "letter: skill letters were not persisted: letter record '1' failed validation."
                .to_owned()
        )]
    );
}

/// An invalid row beside valid rows preserves them: the hook fails without
/// appending, migrating, or deleting anything.
#[tokio::test]
async fn invalid_row_beside_valid_rows_appends_nothing() {
    let registry = registry(&TWO);
    let assembly = drawn(&registry);
    let bad = row(&assembly.chunks[1]).replacen("\"v\":1", "\"v\":2", 1);
    let tap = Tap {
        stored: vec![row(&assembly.chunks[0]), bad],
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry).await;

    assert_eq!(
        result,
        Err(HookError::Failed {
            message: "letter record '2' failed validation".into(),
        })
    );
    assert_eq!(
        tap.ops(),
        [],
        "the valid row is kept and no duplicate id is appended"
    );
}

/// A well-formed row whose id sits outside `1..=20` fails before any write.
#[tokio::test]
async fn id_outside_the_admission_range_fails_before_any_write() {
    let registry = registry(&TWO);
    let assembly = drawn(&registry);
    let bad = row(&assembly.chunks[0]).replacen("\"id\":\"1\"", "\"id\":\"21\"", 1);
    let tap = Tap {
        stored: vec![bad],
        ..Tap::default()
    }
    .shared();
    let result = run(&tap, registry).await;

    assert_eq!(
        result,
        Err(HookError::Failed {
            message: "letter record '21' failed validation".into(),
        })
    );
    assert_eq!(tap.ops(), [], "no blob or record lands before the failure");
}

#[test]
fn prefix_matching_stops_at_a_gap_a_duplicate_or_a_change() {
    let assembly = drawn(&registry(&[
        ("alpha", "first"),
        ("beta", "second"),
        ("gamma", "third"),
    ]));
    let rows = |picks: &[usize]| -> Vec<SkillLetterRecord> {
        picks
            .iter()
            .map(|index| record_of(&assembly.chunks[*index]))
            .collect()
    };

    assert_eq!(matched_prefix(&rows(&[0, 1, 2]), &assembly), 3);
    assert_eq!(
        matched_prefix(&rows(&[0, 1]), &assembly),
        2,
        "a strict prefix"
    );
    assert_eq!(
        matched_prefix(&rows(&[0, 2]), &assembly),
        1,
        "a gap ends the prefix at the row before it"
    );
    assert_eq!(
        matched_prefix(&rows(&[0, 0]), &assembly),
        1,
        "a duplicate id ends the prefix"
    );
    let mut changed = rows(&[0, 1]);
    changed[1].source_blob = BlobId::from_bytes(b"another description");
    assert_eq!(matched_prefix(&changed, &assembly), 1);
    assert_eq!(matched_prefix(&[], &assembly), 0);
}
