//! Host-level receipt proofs for private effects over the loopback Cursor peer.
//!
//! Every test drives one prompt through the real synthetic loop, the real
//! session journal file, and a real HTTP/2 peer on 127.0.0.1. The private effect
//! is a registered tool. It reads its own start receipt from the durable journal
//! before it runs and records what it found, so start durability is observed at
//! the effect rather than inferred afterwards.
//!
//! Three observations carry the proofs:
//! - the effect's log line, written after its start-receipt check;
//! - the peer's probe, which reads the journal at the instant the result bytes
//!   arrive on the wire, so the settled end must already be durable;
//! - the store's rejected batches, which name each batch an injected write
//!   fault refused. The first one is the failed attempt, identified by record kind.
//!
//! Reopen is `Host::open` with `SessionRef::Resume` on a fresh host over the same
//! data root. Recovery repairs an unresolved start by emitting one
//! `PrivateOutcome::Indeterminate` end, and that end is the terminal owner after
//! reopen. No effect replays.
//!
//! Requires `Store::rejected_journal_batches` (dal-store, test-support; drains the
//! rejected batches of one session) and `Wire::probe_upload` (`transport_tests.rs`).

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, Family, ModelId, ModelInfo,
    ModelRequest, ModelRoute, Name, Part, PrivateEffectId, PrivateOutcome, Product as StoreProduct,
    Record, Reply, ServiceSet, SessionId, ThinkingLevel, ToolClass, ToolSpec, TurnEndStop,
    UpdateKind, Workspace,
};
use dal_provider::build_client_http2_prior_knowledge;
use dal_store::Store;
use tokio::{io::AsyncWriteExt as _, time::timeout};

use super::transport_tests::{
    CursorH2, MODEL, PROVIDER, RunScript, SECRET, Step, exec_frame, marker, text_frame,
    turn_ended_frame, uploaded_once_settled,
};
use crate::{
    Agent, Delivery, Env, Host, Product, SessionRef, Subscription,
    ext::{
        BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
        PrivateTool,
        tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput},
    },
};

use crate::ext::synthetic::decision_witness::{self, PrivateDecision};

const DATA_DIR: &str = "data";
const WORKSPACE_DIR: &str = "w";
const EFFECT_LOG: &str = "effects.log";
const EFFECT_TOOL: &str = "record_effect";
const FIRST: &str = "p1";
const TURN_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN: Duration = Duration::from_secs(5);

/// Whether the journal rejects a batch, and when the rejection is armed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Injection {
    /// Every journal batch lands.
    Clean,
    /// The first non-empty batch after the model call returns is rejected. The
    /// rejection sticks for the live journal, so every later batch fails too.
    StartReceipt,
    /// The effect arms the rejection after it confirms its start receipt, so the
    /// first batch rejected is the settled end.
    EndReceipt,
}

/// The file's bytes. A journal that was never written reads as empty.
async fn file_bytes(path: &Path) -> Vec<u8> {
    match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{} is readable: {error}", path.display()),
    }
}

/// Decodes every complete journal line and fails on a malformed one. A line is
/// complete once its newline is written. The unterminated tail is a record still
/// being appended, so it is not a record yet and is excluded by position.
fn decode_journal(bytes: &[u8]) -> Vec<Record> {
    let mut lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    lines.pop();
    lines
        .into_iter()
        .map(|line| {
            dal_core::decode(line)
                .expect("every complete journal line decodes")
                .record
        })
        .collect()
}

async fn read_records(path: &Path) -> Vec<Record> {
    decode_journal(&file_bytes(path).await)
}

/// The start receipts for `call`, as record positions with their effect tokens.
fn starts_for<'a>(records: &'a [Record], call: &str) -> Vec<(usize, &'a PrivateEffectId)> {
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| match record {
            Record::PrivateStart {
                token,
                call: started,
                ..
            } if started.as_str() == call => Some((index, token)),
            _ => None,
        })
        .collect()
}

/// The terminal outcomes recorded for `token`, as record positions in journal order.
fn ends_for<'a>(
    records: &'a [Record],
    token: &PrivateEffectId,
) -> Vec<(usize, &'a PrivateOutcome)> {
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| match record {
            Record::PrivateEnd {
                token: ended,
                outcome,
                ..
            } if ended == token => Some((index, outcome)),
            _ => None,
        })
        .collect()
}

fn is_private_start(record: &Record) -> bool {
    matches!(record, Record::PrivateStart { .. })
}

fn is_private_end(record: &Record) -> bool {
    matches!(record, Record::PrivateEnd { .. })
}

fn is_indeterminate_end(record: &Record) -> bool {
    matches!(
        record,
        Record::PrivateEnd {
            outcome: PrivateOutcome::Indeterminate,
            ..
        }
    )
}

fn count_records(records: &[Record], pick: fn(&Record) -> bool) -> usize {
    records.iter().filter(|record| pick(record)).count()
}

/// For each durable turn end, whether it stopped as a clean finish.
fn turn_ends_done(records: &[Record]) -> Vec<bool> {
    records
        .iter()
        .filter_map(|record| match record {
            Record::TurnEnd { stop, .. } => Some(matches!(stop, TurnEndStop::Done)),
            _ => None,
        })
        .collect()
}

/// Whether `needle` occurs in `haystack`. An empty needle never occurs.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Whether the journal file on disk holds a settled end for the start of `FIRST`.
/// The peer runs this on a blocking thread for the chunk that carries the result,
/// before that chunk is observable, so the end must already be durable.
fn settled_end_on_disk(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let records = decode_journal(&bytes);
    starts_for(&records, FIRST).iter().any(|(_, token)| {
        ends_for(&records, token).iter().any(|(_, outcome)| {
            matches!(
                outcome,
                PrivateOutcome::Settled {
                    is_error: false,
                    ..
                }
            )
        })
    })
}

async fn append_line(path: &Path, line: &str) {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .expect("the effect log opens for append");
    file.write_all(format!("{line}\n").as_bytes())
        .await
        .expect("the effect line is written");
    file.flush().await.expect("the effect line is flushed");
}

/// The effect log's lines. A missing log means no effect ran.
async fn effect_lines(path: &Path) -> Vec<String> {
    let text = String::from_utf8(file_bytes(path).await).expect("the effect log is UTF-8");
    text.lines().map(str::to_owned).collect()
}

/// The registered private effect. It reads its start receipt from the durable
/// journal, records what it found, and returns the marker for its call.
struct RecordEffect {
    spec: Arc<ToolSpec>,
    log: PathBuf,
    store: Store,
    injection: Injection,
}

impl RecordEffect {
    fn new(log: PathBuf, store: Store, injection: Injection) -> Self {
        let spec = Arc::new(ToolSpec {
            name: Name::parse(EFFECT_TOOL).expect("the effect tool name is valid"),
            description: "Appends one evidence line to the effect log.".into(),
            parameters: dal_core::RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("the effect tool schema parses"),
            grammar: None,
        });
        Self {
            spec,
            log,
            store,
            injection,
        }
    }
}

impl Tool for RecordEffect {
    fn name(&self) -> &Name {
        &self.spec.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let session = cx.session();
            let records = read_records(&self.store.session_file(session)).await;
            let call_id = call.id.as_str();
            let receipt = if starts_for(&records, call_id).is_empty() {
                "missing"
            } else {
                "durable"
            };
            append_line(&self.log, &format!("effect {call_id} start {receipt}")).await;
            if self.injection == Injection::EndReceipt {
                self.store.arm_append_write_fault(session, 0);
            }
            ToolOutcome::Ok(Box::new(ToolOutput::from_text(marker(call_id))))
        })
    }
}

/// Routes every turn through the Cursor route with the effect offered as a
/// private tool. Arms the rejection after the model call returns when asked.
struct GateModel {
    effect: Arc<RecordEffect>,
    store: Store,
    session: Arc<OnceLock<SessionId>>,
    injection: Injection,
    armed: AtomicBool,
}

impl ModelHandler for GateModel {
    fn run<'a>(
        &'a self,
        mut request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            request.model = ModelRoute::Api {
                family: Family::Cursor,
                model: MODEL.into(),
            };
            let effect = Arc::clone(&self.effect);
            let private = [PrivateTool(effect)];
            let stream = cx.forward(request, &private).await?;
            if self.injection == Injection::StartReceipt && !self.armed.swap(true, Ordering::SeqCst)
            {
                let session = *self
                    .session
                    .get()
                    .expect("the session is recorded before the prompt");
                self.store.arm_append_write_fault(session, 0);
            }
            Ok(stream)
        })
    }
}

/// The scratch directory and the store that every host in one test shares.
struct Harness {
    scratch: tempfile::TempDir,
    store: Store,
    session: Arc<OnceLock<SessionId>>,
}

impl Harness {
    fn new() -> Self {
        let scratch = tempfile::tempdir().expect("the scratch directory is created");
        let data_root = scratch.path().join(DATA_DIR);
        let workspace_dir = scratch.path().join(WORKSPACE_DIR);
        std::fs::create_dir_all(&data_root).expect("the data directory is created");
        std::fs::create_dir_all(&workspace_dir).expect("the workspace directory is created");
        let store = Store::new(data_root, workspace_at(&workspace_dir), StoreProduct::Dal);
        Self {
            scratch,
            store,
            session: Arc::new(OnceLock::new()),
        }
    }

    fn data_root(&self) -> PathBuf {
        self.scratch.path().join(DATA_DIR)
    }

    fn workspace_dir(&self) -> PathBuf {
        self.scratch.path().join(WORKSPACE_DIR)
    }

    fn effect_log(&self) -> PathBuf {
        self.scratch.path().join(EFFECT_LOG)
    }

    fn journal(&self, session: SessionId) -> PathBuf {
        self.store.session_file(session)
    }
}

fn workspace_at(dir: &Path) -> Workspace {
    Workspace::new(dir.to_path_buf()).expect("the workspace directory is valid")
}

/// Starts a real host over the harness's data root. The Cursor provider points
/// at the loopback peer, and its HTTP/2 client is installed at setup.
fn build_host(harness: &Harness, base_url: &str, injection: Injection) -> Host {
    let data_root = harness.data_root();
    let user = format!(
        "model = \"acme/gate\"\n\n[providers.{PROVIDER}]\napi = \"cursor\"\nbase_url = \"{base_url}\"\nkey_env = \"CURSOR_H2_KEY\"\nauth = \"bearer\"\n\n[aliases]\n{MODEL} = \"{PROVIDER}/{MODEL}\"\n"
    );
    let config = Config::load(ConfigProduct::Dalgon, &data_root, "", Some(user.as_str()))
        .expect("the config loads");
    let model = GateModel {
        effect: Arc::new(RecordEffect::new(
            harness.effect_log(),
            harness.store.clone(),
            injection,
        )),
        store: harness.store.clone(),
        session: Arc::clone(&harness.session),
        injection,
        armed: AtomicBool::new(false),
    };
    let extension = ExtensionBuilder::new("private-receipt-gate", "0.1.0", ServiceSet::default())
        .expect("the extension builder accepts its name")
        .model(ModelRecord {
            id: ModelId::parse("acme/gate").expect("the model id is valid"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(model),
            export: None,
        })
        .build()
        .expect("the extension builds");
    let product = Product {
        name: "dal",
        data_root,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("CURSOR_H2_KEY"), OsString::from(SECRET))]),
        cwd: harness.workspace_dir(),
        sandbox_helper: None,
    };
    Host::start_with_provider_setup(product, config, env, |set| {
        set.set_test_client(PROVIDER, build_client_http2_prior_knowledge())
            .expect("the loopback HTTP/2 client installs before the set is shared");
        Ok(())
    })
    .expect("the host starts with the loopback client")
}

/// Opens a fresh persistent session and records its id for the model handler.
async fn open_fresh(harness: &Harness, host: &Host) -> Agent {
    let agent = host
        .open(
            SessionRef::New {
                workspace: workspace_at(&harness.workspace_dir()),
                name: None,
            },
            ClientId::new("probe"),
        )
        .await
        .expect("the session opens");
    assert!(
        harness.session.set(agent.session()).is_ok(),
        "the session is recorded once"
    );
    agent
}

/// Reopens an existing session on a fresh host. The agent is dropped at once.
async fn reopen(harness: &Harness, host: &Host, session: SessionId) {
    let agent = host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: workspace_at(&harness.workspace_dir()),
            },
            ClientId::new("probe"),
        )
        .await
        .expect("the session reopens on the fresh host");
    drop(agent);
}

async fn submit_prompt(agent: &Agent, text: &str) {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("the session accepts the prompt");
    assert!(matches!(reply, Reply::Accepted { .. }), "{reply:?}");
}

/// Submits one prompt, then reads updates until the turn ends.
async fn prompt_to_end(agent: &Agent, subscription: &mut Subscription, text: &str) {
    submit_prompt(agent, text).await;
    let mut seen = String::new();
    loop {
        let delivery = timeout(TURN_TIMEOUT, subscription.next())
            .await
            .unwrap_or_else(|_| panic!("the turn never ended; saw {seen}"))
            .expect("the update stream stays open");
        let Delivery::Update(update) = delivery else {
            continue;
        };
        let _ = writeln!(seen, "{:?}", update.kind);
        if matches!(update.kind, UpdateKind::TurnEnded { .. }) {
            return;
        }
    }
}

/// Waits until the synthetic loop publishes its branch decision for `call`. The
/// decision is published when the round step that reached the branch ends, after
/// that step's round-level sends. It is a decision witness, not a terminal
/// stream: it claims nothing about later rounds or later calls.
async fn decision_for(session: SessionId, call: &str) -> PrivateDecision {
    timeout(TURN_TIMEOUT, async {
        loop {
            if let Some(decision) = decision_witness::take(session, call) {
                return decision;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the synthetic loop records its decision for the call")
}

/// Polls until the store has rejected a journal batch for `session`, then
/// returns the first one. Each call drains the batches rejected since the last
/// call, in attempt order, so the first element of the first non-empty drain is
/// the first rejected attempt. Each poll is a positive check, not a quiescence wait.
async fn first_rejected_batch(store: &Store, session: SessionId) -> Vec<u8> {
    timeout(TURN_TIMEOUT, async {
        loop {
            if let Some(batch) = store.rejected_journal_batches(session).into_iter().next() {
                return batch;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the armed journal write rejects a batch")
}

/// The start receipt is durable before the effect runs, and the settled end is
/// durable before the result reaches the wire. Reopen adds no private record.
#[tokio::test]
async fn private_effect_runs_after_durable_start_and_its_result_leaves_after_durable_end() {
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame(FIRST, EFFECT_TOOL, 1)),
        Step::AwaitUpload(marker(FIRST).into_bytes()),
        Step::Send(text_frame("done")),
        Step::Send(turn_ended_frame(1, 1)),
    ])])
    .await;
    let harness = Harness::new();
    let host = build_host(&harness, &server.base_url(), Injection::Clean);
    let agent = open_fresh(&harness, &host).await;
    let session = agent.session();
    let journal = harness.journal(session);
    let settled_at_receipt = Arc::new(OnceLock::new());
    let receipt_slot = Arc::clone(&settled_at_receipt);
    let receipt_journal = journal.clone();
    server.wire.probe_upload(
        0,
        marker(FIRST).into_bytes(),
        Box::new(move || {
            let _ = receipt_slot.set(settled_end_on_disk(&receipt_journal));
        }),
    );
    let mut subscription = agent.subscribe(None).expect("the subscription opens");
    prompt_to_end(&agent, &mut subscription, "go").await;
    drop(subscription);
    drop(agent);
    let report = host.shutdown(SHUTDOWN).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "every session task settles before the evidence is read"
    );

    assert_eq!(
        effect_lines(&harness.effect_log()).await,
        [format!("effect {FIRST} start durable")],
        "the effect ran once, after its durable start receipt"
    );
    assert_eq!(
        settled_at_receipt.get(),
        Some(&true),
        "the settled end is durable when the result reaches the wire"
    );
    let records = read_records(&journal).await;
    let starts = starts_for(&records, FIRST);
    let [(start, token)] = starts.as_slice() else {
        panic!("exactly one start receipt is durable");
    };
    let ends = ends_for(&records, token);
    let [(end, outcome)] = ends.as_slice() else {
        panic!("exactly one end closes the start receipt");
    };
    assert!(
        matches!(
            outcome,
            PrivateOutcome::Settled {
                is_error: false,
                ..
            }
        ),
        "the end settles without error"
    );
    assert!(start < end, "the start receipt precedes its end");
    assert_eq!(
        count_records(&records, is_private_start),
        1,
        "one private start is durable"
    );
    assert_eq!(
        count_records(&records, is_private_end),
        1,
        "one private end is durable"
    );
    assert_eq!(
        turn_ends_done(&records),
        [true],
        "the turn ends as a clean finish"
    );
    assert_eq!(
        uploaded_once_settled(&server.wire, 0, &marker(FIRST)).await,
        1,
        "the result reaches the wire exactly once"
    );

    assert_reopen_adds_no_private_record(&harness, &server, session, &journal).await;
    server.shutdown().await;
}

/// Reopens on a fresh host and proves the settled end stays terminal: recovery
/// finds no unresolved start, adds no private record, and replays nothing.
async fn assert_reopen_adds_no_private_record(
    harness: &Harness,
    server: &CursorH2,
    session: SessionId,
    journal: &Path,
) {
    // Reopen on a fresh host. The settled end is the terminal owner, so recovery
    // finds no unresolved start, adds no private record, and replays nothing.
    let host = build_host(harness, &server.base_url(), Injection::Clean);
    reopen(harness, &host, session).await;
    host.shutdown(SHUTDOWN).await;
    let reopened = read_records(journal).await;
    assert_eq!(
        count_records(&reopened, is_private_start),
        1,
        "reopen adds no private start"
    );
    assert_eq!(
        count_records(&reopened, is_private_end),
        1,
        "reopen adds no private end"
    );
    assert_eq!(
        count_records(&reopened, is_indeterminate_end),
        0,
        "the settled end is not reclassified on reopen"
    );
    assert_eq!(
        effect_lines(&harness.effect_log()).await.len(),
        1,
        "reopen replays nothing"
    );
}

/// The first journal batch after the model returns is p1's start receipt, and the
/// rejection sticks. No effect runs, no result reaches the wire, and a reopen
/// finds no start row, so it emits no end and replays nothing.
#[tokio::test]
async fn rejected_start_receipt_runs_no_effect_and_reopen_owns_no_end() {
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame(FIRST, EFFECT_TOOL, 1)),
        Step::Hold,
    ])])
    .await;
    let harness = Harness::new();
    let host = build_host(&harness, &server.base_url(), Injection::StartReceipt);
    let agent = open_fresh(&harness, &host).await;
    let session = agent.session();
    submit_prompt(&agent, "go").await;
    let rejected = first_rejected_batch(&harness.store, session).await;
    let decision = decision_for(session, FIRST).await;
    assert_eq!(
        decision,
        PrivateDecision::StartRejected,
        "the branch decides the start rejection"
    );
    drop(agent);
    timeout(TURN_TIMEOUT, host.close(session))
        .await
        .expect("the failed session closes within the turn timeout")
        .expect("the failed session closes once its actor settles");
    let report = host.shutdown(SHUTDOWN).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "every session task settles before the evidence is read"
    );
    assert_eq!(
        report.sessions_closed, 0,
        "close already settled the session"
    );

    let rejected_records = decode_journal(&rejected);
    assert!(
        rejected_records.iter().any(|record| matches!(
            record,
            Record::PrivateStart { call, .. } if call.as_str() == FIRST
        )),
        "the first rejected batch is p1's start receipt: {rejected_records:?}"
    );
    assert!(
        effect_lines(&harness.effect_log()).await.is_empty(),
        "the effect never ran"
    );
    assert!(
        !contains(&server.wire.upload(0), marker(FIRST).as_bytes()),
        "no result for p1 reaches the wire"
    );
    let journal = harness.journal(session);
    assert_eq!(
        count_records(&read_records(&journal).await, is_private_start),
        0,
        "no start receipt for p1 is durable"
    );

    // Reopen on a fresh host. With no durable start row, recovery emits no end.
    let host = build_host(&harness, &server.base_url(), Injection::Clean);
    reopen(&harness, &host, session).await;
    host.shutdown(SHUTDOWN).await;
    let reopened = read_records(&journal).await;
    assert_eq!(
        count_records(&reopened, is_private_end),
        0,
        "reopen emits no end for p1"
    );
    assert!(
        effect_lines(&harness.effect_log()).await.is_empty(),
        "reopen replays nothing"
    );
    server.shutdown().await;
}

/// The effect confirms its durable start, then arms the rejection. The first
/// batch it rejects is p1's settled end. The start stays unresolved, so reopen
/// repairs it with exactly one Indeterminate end, and the effect does not replay.
#[tokio::test]
async fn rejected_end_receipt_reopens_with_one_indeterminate_end_and_no_replay() {
    let server = CursorH2::start(vec![RunScript::new(vec![
        Step::Send(exec_frame(FIRST, EFFECT_TOOL, 1)),
        Step::Hold,
    ])])
    .await;
    let harness = Harness::new();
    let host = build_host(&harness, &server.base_url(), Injection::EndReceipt);
    let agent = open_fresh(&harness, &host).await;
    let session = agent.session();
    submit_prompt(&agent, "go").await;
    let rejected = first_rejected_batch(&harness.store, session).await;
    let decision = decision_for(session, FIRST).await;
    assert_eq!(
        decision,
        PrivateDecision::EndRejected,
        "the branch decides the end rejection"
    );
    drop(agent);
    timeout(TURN_TIMEOUT, host.close(session))
        .await
        .expect("the failed session closes within the turn timeout")
        .expect("the failed session closes once its actor settles");
    let report = host.shutdown(SHUTDOWN).await;
    assert_eq!(
        report.tasks_remaining, 0,
        "every session task settles before the evidence is read"
    );
    assert_eq!(
        report.sessions_closed, 0,
        "close already settled the session"
    );

    let journal = harness.journal(session);
    let durable = read_records(&journal).await;
    let starts = starts_for(&durable, FIRST);
    let [(_, token)] = starts.as_slice() else {
        panic!("the start receipt is durable before the effect");
    };
    let rejected_records = decode_journal(&rejected);
    assert!(
        rejected_records.iter().any(|record| matches!(
            record,
            Record::PrivateEnd { token: ended, outcome: PrivateOutcome::Settled { .. }, .. }
                if ended == *token
        )),
        "the first rejected batch is p1's settled end: {rejected_records:?}"
    );
    assert_eq!(
        effect_lines(&harness.effect_log()).await,
        [format!("effect {FIRST} start durable")],
        "the effect ran once, after its durable start receipt"
    );
    assert!(
        !contains(&server.wire.upload(0), marker(FIRST).as_bytes()),
        "the unsettled result never reaches the wire"
    );
    assert!(
        ends_for(&durable, token).is_empty(),
        "no end is durable before reopen"
    );

    // Reopen on a fresh host. Recovery repairs the unresolved start with exactly
    // one Indeterminate end, and the effect does not replay.
    let host = build_host(&harness, &server.base_url(), Injection::Clean);
    reopen(&harness, &host, session).await;
    host.shutdown(SHUTDOWN).await;
    let reopened = read_records(&journal).await;
    let reopened_starts = starts_for(&reopened, FIRST);
    let [(_, reopened_token)] = reopened_starts.as_slice() else {
        panic!("the start receipt survives reopen");
    };
    let owners = ends_for(&reopened, reopened_token);
    let [(_, owner)] = owners.as_slice() else {
        panic!("exactly one end owns the start after reopen");
    };
    assert!(
        matches!(owner, PrivateOutcome::Indeterminate),
        "reopen repairs the unresolved start as Indeterminate"
    );
    assert_eq!(
        effect_lines(&harness.effect_log()).await.len(),
        1,
        "reopen replays nothing"
    );
    server.shutdown().await;
}
