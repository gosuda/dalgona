//! Stored images and stored text return to the provider request as inline
//! content.
//!
//! Images of 16 KiB or more leave the journal for the session blob store.
//! The request boundary in `session::turn` reads them back before any
//! route runs, so reopened, forked, and cloned sessions submit the same
//! bytes the live turn sent. Absent, damaged, or misrecorded blobs refuse
//! the turn instead of sending a partial request. A text part of 16 KiB or
//! more spills the same way and returns as inline text; a text blob that is
//! absent, damaged, misrecorded, unreadable, or not UTF-8 refuses the turn too.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test assertions abort on failure")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    AssistantPart, BlobId, Caps, ClientId, Command, Config, ConfigProduct, ContentLimits,
    ContextItem, Entry, EntryId, EntryKind, Expect, JournalPart, ModelId, ModelRequest, Output,
    Part, Product as JournalProduct, Record, Reply, ServiceSet, SessionId, Stop, ThinkingLevel,
    TurnId, UpdateKind, Usage, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent as ProviderEvent};
use dal_store::Store;

/// How long any one test step may take before the test fails.
const WAIT: Duration = Duration::from_secs(30);

/// One scripted reply the recorder never needs; the recorder answers itself.
const SCRIPT_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"unused\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Every request the registered model received, in arrival order.
#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<ModelRequest>>>);

impl Requests {
    fn push(&self, request: ModelRequest) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
    }

    fn len(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn first(&self) -> ModelRequest {
        self.get(0)
    }

    fn get(&self, index: usize) -> ModelRequest {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[index]
            .clone()
    }
}

/// Records each request it receives and ends the turn with `ok`.
#[derive(Clone)]
struct Recorder {
    requests: Requests,
}

impl ModelHandler for Recorder {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.requests.push(request);
            Ok(done_stream())
        })
    }
}

fn done_stream() -> EventStream {
    let usage = Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
        premium_requests: None,
    };
    let items: Vec<Result<ProviderEvent, ProviderError>> = vec![
        Ok(ProviderEvent::TextDelta { text: "ok".into() }),
        Ok(ProviderEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(ProviderEvent::Usage { usage }),
        Ok(ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ];
    EventStream::new(futures::stream::iter(items), || {})
}

fn caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: true,
        custom_grammar: false,
    }
}

/// Test directories, workspace, and the request log shared across restarts.
struct Rig {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    workspace_dir: PathBuf,
    workspace: Workspace,
    requests: Requests,
}

fn rig() -> Rig {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    Rig {
        _tmp: tmp,
        data,
        workspace_dir,
        workspace,
        requests: Requests::default(),
    }
}

async fn start_host(rig: &Rig) -> Host {
    let fixture = rig.data.join("script.jsonl");
    std::fs::write(&fixture, SCRIPT_STEP).expect("provider fixture");
    let user = format!(
        "model = \"acme/recorder\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &rig.data, "", Some(user.as_str())).expect("config");
    let extension = ExtensionBuilder::new("hydro", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .model(ModelRecord {
            id: ModelId::parse("acme/recorder").expect("model id"),
            caps: caps(),
            handler: Arc::new(Recorder {
                requests: rig.requests.clone(),
            }),
            export: None,
        })
        .build()
        .expect("recorder extension");
    let product = Product {
        name: "dal",
        data_root: rig.data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: rig.workspace_dir.clone(),
        sandbox_helper: None,
    };
    Host::start(product, config, env).await.expect("host")
}

async fn open_new(host: &Host, workspace: &Workspace) -> Agent {
    host.open(
        SessionRef::New {
            workspace: workspace.clone(),
            name: None,
        },
        ClientId::new("hydration"),
    )
    .await
    .expect("open session")
}

async fn resume(host: &Host, workspace: &Workspace, id: SessionId) -> Agent {
    host.open(
        SessionRef::Resume {
            key: id.to_string().into(),
            workspace: workspace.clone(),
        },
        ClientId::new("hydration"),
    )
    .await
    .expect("resume session")
}

/// Submits one prompt and returns its turn and user entry.
async fn prompt(agent: &Agent, content: Vec<Part>) -> (TurnId, EntryId) {
    match agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content,
        })
        .await
        .expect("submit prompt")
    {
        Reply::Accepted { turn, message_id } => (turn, message_id),
        other => panic!("prompt accepted, got {other:?}"),
    }
}

/// What one awaited turn reported.
struct TurnOutcome {
    stop: Stop,
    notices: Vec<(String, String)>,
}

/// Reads updates until `turn` ends, keeping every notice kind and text.
async fn wait_end(subscription: &mut dal_agent::Subscription, turn: TurnId) -> TurnOutcome {
    let mut notices = Vec::new();
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .expect("update arrives in time");
        let Some(delivery) = delivery else {
            panic!("subscription closed before turn {turn} ended");
        };
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::Notice(notice) => {
                notices.push((notice.kind.to_string(), notice.text.to_string()));
            }
            UpdateKind::TurnEnded { turn: ended, stop } if *ended == turn => {
                return TurnOutcome {
                    stop: *stop,
                    notices,
                };
            }
            _ => {}
        }
    }
}

/// Forks or clones `agent`, returning the new session identity.
async fn branched(agent: &Agent, command: Command) -> SessionId {
    match agent.submit(command).await.expect("branch accepted") {
        Reply::Done(Output::Text(text)) => SessionId::parse(&text).expect("child session id"),
        other => panic!("branch reports the child id, got {other:?}"),
    }
}

/// One message part as the submitted request carried it.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Text(String),
    Image { mime: String, bytes: Vec<u8> },
    Reply(String),
    Stored(String),
}

/// One submitted part as the request carried it.
fn seen_part(part: &Part) -> Seen {
    match part {
        Part::Text { text } => Seen::Text(text.to_string()),
        Part::Image { mime, bytes } => Seen::Image {
            mime: mime.to_string(),
            bytes: bytes.to_vec(),
        },
        Part::Blob { blob_id, .. } => Seen::Stored(blob_id.to_string()),
    }
}

/// The assistant text of one assistant message.
fn reply_text(parts: &[AssistantPart]) -> String {
    let mut text = String::new();
    for part in parts {
        if let AssistantPart::Text { text: delta, .. } = part {
            text.push_str(delta);
        }
    }
    text
}

/// Flattens a submitted request to its message parts in order.
fn seen_of(request: &ModelRequest) -> Vec<Seen> {
    let mut out = Vec::new();
    for item in request.context.iter() {
        match item {
            ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => {
                out.extend(parts.iter().map(seen_part));
            }
            ContextItem::Assistant { parts, .. } => {
                out.push(Seen::Reply(reply_text(parts)));
            }
        }
    }
    out
}

/// Deterministic image bytes; distinct seeds never collide in a test.
fn image_bytes(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    let mut state = u32::from(seed).wrapping_add(0x9e37);
    for _ in 0..len {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        bytes.push((state >> 16).to_le_bytes()[0]);
    }
    bytes
}

/// Fixture images: above the spill threshold, exactly on it, and inline.
struct Images {
    big: Vec<u8>,
    edge: Vec<u8>,
    small: Vec<u8>,
}

fn images() -> Images {
    Images {
        big: image_bytes(20_480, 7),
        edge: image_bytes(ContentLimits::INLINE_BYTES, 9),
        small: image_bytes(64, 3),
    }
}

fn prompt_one(img: &Images) -> Vec<Part> {
    vec![
        Part::Text {
            text: "describe these images".into(),
        },
        Part::Image {
            mime: "image/png".into(),
            bytes: img.big.clone().into_boxed_slice(),
        },
        Part::Image {
            mime: "image/jpeg".into(),
            bytes: img.edge.clone().into_boxed_slice(),
        },
        Part::Image {
            mime: "image/gif".into(),
            bytes: img.small.clone().into_boxed_slice(),
        },
    ]
}

fn expected_first(img: &Images) -> Vec<Seen> {
    vec![
        Seen::Text("describe these images".to_owned()),
        Seen::Image {
            mime: "image/png".to_owned(),
            bytes: img.big.clone(),
        },
        Seen::Image {
            mime: "image/jpeg".to_owned(),
            bytes: img.edge.clone(),
        },
        Seen::Image {
            mime: "image/gif".to_owned(),
            bytes: img.small.clone(),
        },
    ]
}

fn expected_after(img: &Images, follow_up: &str) -> Vec<Seen> {
    let mut seen = expected_first(img);
    seen.push(Seen::Reply("ok".to_owned()));
    seen.push(Seen::Text(follow_up.to_owned()));
    seen
}

/// The blob file for `id` under the data root, when the store spilled it.
fn blob_file(data: &Path, id: BlobId) -> Option<PathBuf> {
    let name = id.to_string();
    let mut stack = vec![data.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries {
            let path = entry.ok()?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && path.file_name().and_then(|file| file.to_str()) == Some(name.as_str())
            {
                return Some(path);
            }
        }
    }
    None
}

fn entry_id(n: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(n).expect("entry id"))
}

fn user_record(id: u64, parent: Option<EntryId>, parts: Vec<JournalPart>) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent,
        at: dal_core::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User { parts },
    })
}

fn blob_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).expect("fixture length fits")
}

#[tokio::test]
async fn reopened_session_resends_spilled_images_inline() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (turn, _) = prompt(&agent, prompt_one(&img)).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert_eq!(rig.requests.len(), 1);
    assert_eq!(seen_of(&rig.requests.first()), expected_first(&img));

    assert!(
        blob_file(&rig.data, BlobId::from_bytes(&img.big)).is_some(),
        "the 20 KiB image spills to the blob store"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(&img.edge)).is_some(),
        "the threshold image spills to the blob store"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(&img.small)).is_none(),
        "the small image stays inline"
    );

    host.close(id).await.expect("close session");
    drop(agent);
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(
        &agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert_eq!(rig.requests.len(), 2);
    assert_eq!(seen_of(&rig.requests.get(1)), expected_after(&img, "again"));
    host.shutdown(Duration::from_secs(1)).await;
}

/// The spill boundary from below: an image one byte under the threshold
/// stays inline in the journal while one exactly at it leaves for the blob
/// store, and the provider receives both byte for byte.
#[tokio::test]
async fn an_image_one_byte_below_the_threshold_stays_inline() {
    let rig = rig();
    let below = image_bytes(ContentLimits::INLINE_BYTES - 1, 5);
    let at = image_bytes(ContentLimits::INLINE_BYTES, 6);
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (turn, _) = prompt(
        &agent,
        vec![
            Part::Text {
                text: "boundary".into(),
            },
            Part::Image {
                mime: "image/png".into(),
                bytes: below.clone().into_boxed_slice(),
            },
            Part::Image {
                mime: "image/jpeg".into(),
                bytes: at.clone().into_boxed_slice(),
            },
        ],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(&at)).is_some(),
        "an image exactly at the threshold spills to the blob store"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(&below)).is_none(),
        "an image one byte under the threshold stays inline"
    );
    assert_eq!(
        seen_of(&rig.requests.first()),
        vec![
            Seen::Text("boundary".to_owned()),
            Seen::Image {
                mime: "image/png".to_owned(),
                bytes: below,
            },
            Seen::Image {
                mime: "image/jpeg".to_owned(),
                bytes: at,
            },
        ]
    );
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn restarted_host_resends_spilled_images_inline() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (turn, _) = prompt(&agent, prompt_one(&img)).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert_eq!(seen_of(&rig.requests.first()), expected_first(&img));

    host.close(id).await.expect("close session");
    drop(agent);
    host.shutdown(Duration::from_secs(1)).await;

    let host = start_host(&rig).await;
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(
        &agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert_eq!(rig.requests.len(), 2);
    assert_eq!(seen_of(&rig.requests.get(1)), expected_after(&img, "again"));
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn forked_session_keeps_prompt_one_images() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (first, _) = prompt(&agent, prompt_one(&img)).await;
    assert_eq!(wait_end(&mut subscription, first).await.stop, Stop::EndTurn);
    let (second, second_entry) = prompt(
        &agent,
        vec![Part::Text {
            text: "second".into(),
        }],
    )
    .await;
    assert_eq!(
        wait_end(&mut subscription, second).await.stop,
        Stop::EndTurn
    );

    let child = branched(&agent, Command::Fork(second_entry)).await;
    let child_agent = resume(&host, &rig.workspace, child).await;
    let mut child_subscription = child_agent.subscribe(None).expect("subscribe");
    let (third, _) = prompt(
        &child_agent,
        vec![Part::Text {
            text: "once more".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut child_subscription, third).await;
    assert_eq!(outcome.stop, Stop::EndTurn);

    assert_eq!(rig.requests.len(), 3);
    let mut expected = expected_first(&img);
    expected.push(Seen::Reply("ok".to_owned()));
    expected.push(Seen::Text("once more".to_owned()));
    assert_eq!(seen_of(&rig.requests.get(2)), expected);
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn cloned_session_keeps_spilled_images() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (first, _) = prompt(&agent, prompt_one(&img)).await;
    assert_eq!(wait_end(&mut subscription, first).await.stop, Stop::EndTurn);

    let child = branched(&agent, Command::Clone).await;
    let child_agent = resume(&host, &rig.workspace, child).await;
    let mut child_subscription = child_agent.subscribe(None).expect("subscribe");
    let (second, _) = prompt(
        &child_agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut child_subscription, second).await;
    assert_eq!(outcome.stop, Stop::EndTurn);

    assert_eq!(rig.requests.len(), 2);
    assert_eq!(seen_of(&rig.requests.get(1)), expected_after(&img, "again"));
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn missing_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (turn, _) = prompt(&agent, prompt_one(&img)).await;
    assert_eq!(wait_end(&mut subscription, turn).await.stop, Stop::EndTurn);
    host.close(id).await.expect("close session");
    drop(agent);

    let missing = BlobId::from_bytes(&img.big);
    let path = blob_file(&rig.data, missing).expect("spilled image file");
    std::fs::remove_file(&path).expect("delete spilled image");

    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(
        &agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::Failed);
    assert_eq!(rig.requests.len(), 1, "no request leaves without the image");
    assert!(
        outcome
            .notices
            .iter()
            .any(|(kind, text)| kind.as_str() == "turn.failed"
                && text.contains(&missing.to_string())
                && text.contains("no request was sent")),
        "the failure names the missing blob: {:?}",
        outcome.notices
    );
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn corrupt_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let img = images();
    let host = start_host(&rig).await;
    let agent = open_new(&host, &rig.workspace).await;
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");

    let (turn, _) = prompt(&agent, prompt_one(&img)).await;
    assert_eq!(wait_end(&mut subscription, turn).await.stop, Stop::EndTurn);
    host.close(id).await.expect("close session");
    drop(agent);

    let damaged = BlobId::from_bytes(&img.edge);
    let path = blob_file(&rig.data, damaged).expect("spilled image file");
    let corrupt: Vec<u8> = img.edge.iter().map(|byte| byte.wrapping_add(1)).collect();
    std::fs::remove_file(&path).expect("remove damaged image");
    std::fs::write(&path, &corrupt).expect("write damaged image");

    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(
        &agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::Failed);
    assert_eq!(
        rig.requests.len(),
        1,
        "no request leaves with a damaged image"
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|(kind, text)| kind.as_str() == "turn.failed"
                && text.contains(&damaged.to_string())),
        "the failure names the damaged blob: {:?}",
        outcome.notices
    );
    host.shutdown(Duration::from_secs(1)).await;
}

/// Seeds a session whose second user entry names the published `bytes`.
///
/// `parts` receives the published digest and returns that entry's parts, so
/// a test can record any media type or length for the same stored bytes.
async fn seed_stored_blob(
    data: &Path,
    workspace: &Workspace,
    bytes: &[u8],
    parts: impl FnOnce(&str) -> Vec<JournalPart>,
) -> SessionId {
    let session = SessionId::new_v7();
    let store = Store::new(data.to_path_buf(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user_record(
            1,
            None,
            vec![JournalPart::Text {
                text: "seed".into(),
            }],
        )])
        .await
        .expect("seed first user");
    let published = journal.put_blob(bytes.to_vec()).expect("publish blob");
    assert_eq!(published, BlobId::from_bytes(bytes));
    journal
        .append(vec![user_record(
            2,
            Some(entry_id(1)),
            parts(&published.to_string()),
        )])
        .await
        .expect("seed stored blob");
    journal.close().await.expect("close seeded session");
    session
}

/// Seeds a session whose journal records a wrong length for a real blob.
async fn seed_length_mismatch(
    data: &Path,
    workspace: &Workspace,
    bytes: &[u8],
    recorded: u64,
) -> SessionId {
    seed_stored_blob(data, workspace, bytes, |blob| {
        vec![
            JournalPart::Text {
                text: "look".into(),
            },
            JournalPart::Blob {
                mime: "image/png".into(),
                blob: blob.into(),
                bytes: recorded,
            },
        ]
    })
    .await
}

#[tokio::test]
async fn misrecorded_blob_length_refuses_the_turn_without_sending() {
    let rig = rig();
    let bytes = image_bytes(20_480, 11);
    let recorded = blob_len(&bytes) + 100;
    let id = seed_length_mismatch(&rig.data, &rig.workspace, &bytes, recorded).await;
    let blob = BlobId::from_bytes(&bytes);

    let host = start_host(&rig).await;
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(&agent, vec![Part::Text { text: "go".into() }]).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::Failed);
    assert!(
        rig.requests.is_empty(),
        "no request leaves with a misrecorded image"
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|(kind, text)| kind.as_str() == "turn.failed"
                && text.contains(&blob.to_string())
                && text.contains("holds")),
        "the failure reports the length mismatch: {:?}",
        outcome.notices
    );
    host.shutdown(Duration::from_secs(1)).await;
}

/// Text fixtures: above the spill threshold, exactly on it, and inline. Each
/// is a distinct letter so a swapped or truncated part cannot match.
struct Texts {
    big: String,
    edge: String,
    below: String,
    small: String,
}

fn texts() -> Texts {
    Texts {
        big: "B".repeat(20_000),
        edge: "E".repeat(ContentLimits::INLINE_BYTES),
        below: "b".repeat(ContentLimits::INLINE_BYTES - 1),
        small: "s".repeat(64),
    }
}

/// Seeds one resumable session whose first user entry carries `parts`.
async fn seed_user_parts(data: &Path, workspace: &Workspace, parts: Vec<JournalPart>) -> SessionId {
    let session = SessionId::new_v7();
    let store = Store::new(data.to_path_buf(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user_record(1, None, parts)])
        .await
        .expect("seed user entry");
    journal.close().await.expect("close seeded session");
    session
}

#[tokio::test]
async fn reopened_session_resends_spilled_text_inline() {
    let rig = rig();
    let text = texts();
    let id = seed_user_parts(
        &rig.data,
        &rig.workspace,
        vec![
            JournalPart::Text {
                text: text.big.clone().into(),
            },
            JournalPart::Text {
                text: text.edge.clone().into(),
            },
            JournalPart::Text {
                text: text.below.clone().into(),
            },
            JournalPart::Text {
                text: text.small.clone().into(),
            },
        ],
    )
    .await;
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(text.big.as_bytes())).is_some(),
        "the 20 KB text spills to the blob store"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(text.edge.as_bytes())).is_some(),
        "the threshold text spills to the blob store"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(text.below.as_bytes())).is_none(),
        "text one byte under the threshold stays inline"
    );
    assert!(
        blob_file(&rig.data, BlobId::from_bytes(text.small.as_bytes())).is_none(),
        "the small text stays inline"
    );

    let host = start_host(&rig).await;
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(&agent, vec![Part::Text { text: "go".into() }]).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn, "{:?}", outcome.notices);
    assert_eq!(rig.requests.len(), 1);
    assert_eq!(
        seen_of(&rig.requests.first()),
        vec![
            Seen::Text(text.big),
            Seen::Text(text.edge),
            Seen::Text(text.below),
            Seen::Text(text.small),
            Seen::Text("go".to_owned()),
        ],
        "every spilled text part reaches the model as the text the user sent"
    );
    host.shutdown(Duration::from_secs(1)).await;
}

/// Seeds a session whose only user entry names a text blob holding `bytes`.
async fn seed_text_blob(data: &Path, workspace: &Workspace, bytes: &[u8]) -> (SessionId, BlobId) {
    seed_text_blob_recording(data, workspace, bytes, blob_len(bytes)).await
}

/// Seeds a text blob entry whose journal part records `recorded` bytes.
async fn seed_text_blob_recording(
    data: &Path,
    workspace: &Workspace,
    bytes: &[u8],
    recorded: u64,
) -> (SessionId, BlobId) {
    seed_blob_part(data, workspace, bytes, |blob| JournalPart::TextBlob {
        blob: blob.into(),
        bytes: recorded,
    })
    .await
}

/// Seeds a session whose only user entry is the part `part` builds for the
/// published digest of `bytes`.
async fn seed_blob_part(
    data: &Path,
    workspace: &Workspace,
    bytes: &[u8],
    part: impl FnOnce(&str) -> JournalPart,
) -> (SessionId, BlobId) {
    let session = SessionId::new_v7();
    let store = Store::new(data.to_path_buf(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user_record(
            1,
            None,
            vec![JournalPart::Text {
                text: "seed".into(),
            }],
        )])
        .await
        .expect("seed first user");
    let published = journal.put_blob(bytes.to_vec()).expect("publish text blob");
    assert_eq!(published, BlobId::from_bytes(bytes));
    journal
        .append(vec![user_record(
            2,
            Some(entry_id(1)),
            vec![part(&published.to_string())],
        )])
        .await
        .expect("seed text blob entry");
    journal.close().await.expect("close seeded session");
    (session, published)
}

/// Resumes `id` and sends one more prompt: the turn must fail naming `blob`
/// and no provider request may leave.
async fn assert_text_refusal(rig: &Rig, id: SessionId, blob: BlobId) {
    let host = start_host(rig).await;
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(&agent, vec![Part::Text { text: "go".into() }]).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::Failed);
    assert!(
        rig.requests.is_empty(),
        "no request leaves with an unusable text blob"
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|(kind, text)| kind.as_str() == "turn.failed" && text.contains(&blob.to_string())),
        "the failure names the blob: {:?}",
        outcome.notices
    );
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn absent_text_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let text = "T".repeat(20_000);
    let (id, missing) = seed_text_blob(&rig.data, &rig.workspace, text.as_bytes()).await;
    let path = blob_file(&rig.data, missing).expect("published text blob file");
    std::fs::remove_file(&path).expect("delete text blob");

    assert_text_refusal(&rig, id, missing).await;
}

#[tokio::test]
async fn misrecorded_text_blob_length_refuses_the_turn_without_sending() {
    let rig = rig();
    let text = "M".repeat(20_000);
    let recorded = blob_len(text.as_bytes()) + 100;
    let (id, blob) =
        seed_text_blob_recording(&rig.data, &rig.workspace, text.as_bytes(), recorded).await;

    assert_text_refusal(&rig, id, blob).await;
}

#[tokio::test]
async fn corrupt_text_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let text = "C".repeat(20_000);
    let (id, damaged) = seed_text_blob(&rig.data, &rig.workspace, text.as_bytes()).await;
    let path = blob_file(&rig.data, damaged).expect("published text blob file");
    std::fs::remove_file(&path).expect("remove damaged text blob");
    std::fs::write(&path, "D".repeat(text.len())).expect("write same-length damaged text");

    assert_text_refusal(&rig, id, damaged).await;
}

#[tokio::test]
async fn unreadable_text_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let text = "U".repeat(20_000);
    let (id, blob) = seed_text_blob(&rig.data, &rig.workspace, text.as_bytes()).await;
    let path = blob_file(&rig.data, blob).expect("published text blob file");
    std::fs::remove_file(&path).expect("remove text blob");
    std::fs::create_dir(&path).expect("replace text blob with a directory");

    assert_text_refusal(&rig, id, blob).await;
}

#[tokio::test]
async fn text_blob_that_is_not_utf8_refuses_the_turn_without_sending() {
    let rig = rig();
    let (id, damaged) = seed_text_blob(&rig.data, &rig.workspace, &vec![0xff; 20_000]).await;

    assert_text_refusal(&rig, id, damaged).await;
}

#[tokio::test]
async fn text_blobs_of_any_text_media_type_resend_inline() {
    for mime in ["text/markdown", "text/plain; charset=utf-8"] {
        let rig = rig();
        let text = "m".repeat(20_000);
        let (id, _) = seed_blob_part(&rig.data, &rig.workspace, text.as_bytes(), |blob| {
            JournalPart::Blob {
                mime: mime.into(),
                blob: blob.into(),
                bytes: blob_len(text.as_bytes()),
            }
        })
        .await;

        let host = start_host(&rig).await;
        let agent = resume(&host, &rig.workspace, id).await;
        let mut subscription = agent.subscribe(None).expect("subscribe");
        let (turn, _) = prompt(&agent, vec![Part::Text { text: "go".into() }]).await;
        let outcome = wait_end(&mut subscription, turn).await;
        assert_eq!(outcome.stop, Stop::EndTurn, "{mime}: {:?}", outcome.notices);
        assert_eq!(
            seen_of(&rig.requests.first()),
            vec![
                Seen::Text("seed".to_owned()),
                Seen::Text(text),
                Seen::Text("go".to_owned()),
            ],
            "{mime} reaches the model as inline text"
        );
        host.shutdown(Duration::from_secs(1)).await;
    }
}

/// Seeds dropped, kept, compaction, and latest entries carrying images.
async fn seed_compaction(data: &Path, workspace: &Workspace) -> (SessionId, Images) {
    use base64::Engine as _;
    let img = images();
    let session = SessionId::new_v7();
    let store = Store::new(data.to_path_buf(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user_record(
            1,
            None,
            vec![JournalPart::Text {
                text: "dropped".into(),
            }],
        )])
        .await
        .expect("seed dropped prefix");
    let kept = journal
        .put_blob(img.big.clone())
        .expect("publish kept image");
    assert_eq!(kept, BlobId::from_bytes(&img.big));
    let carried = journal
        .put_blob(img.edge.clone())
        .expect("publish carried image");
    assert_eq!(carried, BlobId::from_bytes(&img.edge));
    let first = entry_id(1);
    let second = entry_id(2);
    let compaction = entry_id(3);
    journal
        .append(vec![
            user_record(
                2,
                Some(first),
                vec![
                    JournalPart::Text {
                        text: "kept".into(),
                    },
                    JournalPart::Blob {
                        mime: "image/png".into(),
                        blob: kept.to_string().into(),
                        bytes: blob_len(&img.big),
                    },
                ],
            ),
            Record::Compaction(Entry {
                id: compaction,
                parent: Some(second),
                at: dal_core::Timestamp::UNIX_EPOCH,
                kind: EntryKind::Compaction {
                    summary: Some("carried history summary".into()),
                    first_kept: Some(second),
                    tokens_before: 40_000,
                    replay: None,
                    usage: None,
                    parts: vec![
                        JournalPart::Text {
                            text: "carried".into(),
                        },
                        JournalPart::Image {
                            mime: "image/webp".into(),
                            base64: base64::engine::general_purpose::STANDARD
                                .encode(&img.edge)
                                .into(),
                        },
                    ],
                    parts_tokens: 0,
                },
            }),
            user_record(
                4,
                Some(compaction),
                vec![
                    JournalPart::Text {
                        text: "latest".into(),
                    },
                    JournalPart::Image {
                        mime: "image/gif".into(),
                        base64: base64::engine::general_purpose::STANDARD
                            .encode(&img.small)
                            .into(),
                    },
                ],
            ),
        ])
        .await
        .expect("seed kept history");
    journal.close().await.expect("close seeded session");
    let _ = carried;
    (session, img)
}

#[tokio::test]
async fn seeded_compaction_images_hydrate_in_order() {
    let rig = rig();
    let (id, img) = seed_compaction(&rig.data, &rig.workspace).await;
    let host = start_host(&rig).await;
    let agent = resume(&host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(&agent, vec![Part::Text { text: "go".into() }]).await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::EndTurn);
    assert_eq!(rig.requests.len(), 1);
    assert_eq!(
        seen_of(&rig.requests.get(0)),
        vec![
            Seen::Text("carried".to_owned()),
            Seen::Image {
                mime: "image/webp".to_owned(),
                bytes: img.edge.clone(),
            },
            Seen::Text("kept".to_owned()),
            Seen::Image {
                mime: "image/png".to_owned(),
                bytes: img.big.clone(),
            },
            Seen::Text("latest".to_owned()),
            Seen::Image {
                mime: "image/gif".to_owned(),
                bytes: img.small.clone(),
            },
            Seen::Text("go".to_owned()),
        ]
    );
    host.shutdown(Duration::from_secs(1)).await;
}

/// UTF-8 text past the spill threshold, with multi-byte characters throughout.
fn spilled_text() -> String {
    let line = "na\u{ef}ve caf\u{e9} \u{2014} \u{65e5}\u{672c}\u{8a9e} \u{2713}\n";
    line.repeat(ContentLimits::INLINE_BYTES / line.len() + 2)
}

/// Spills `text` through a live prompt, then closes the session.
async fn spill_text_session(rig: &Rig, host: &Host, text: &str) -> SessionId {
    let agent = open_new(host, &rig.workspace).await;
    let id = agent.session();
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(&agent, vec![Part::Text { text: text.into() }]).await;
    assert_eq!(wait_end(&mut subscription, turn).await.stop, Stop::EndTurn);
    host.close(id).await.expect("close session");
    drop(agent);
    id
}

/// Resumes `id` and sends one more prompt; the turn must fail naming
/// `blob`, and no provider request may leave.
async fn assert_turn_refused(rig: &Rig, host: &Host, id: SessionId, blob: BlobId) {
    let sent = rig.requests.len();
    let agent = resume(host, &rig.workspace, id).await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    let (turn, _) = prompt(
        &agent,
        vec![Part::Text {
            text: "again".into(),
        }],
    )
    .await;
    let outcome = wait_end(&mut subscription, turn).await;
    assert_eq!(outcome.stop, Stop::Failed);
    assert_eq!(
        rig.requests.len(),
        sent,
        "no request leaves without the text"
    );
    assert!(
        outcome
            .notices
            .iter()
            .any(|(kind, text)| kind.as_str() == "turn.failed" && text.contains(&blob.to_string())),
        "the failure names the blob: {:?}",
        outcome.notices
    );
}

#[tokio::test]
async fn missing_text_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let text = spilled_text();
    let host = start_host(&rig).await;
    let id = spill_text_session(&rig, &host, &text).await;

    let missing = BlobId::from_bytes(text.as_bytes());
    let path = blob_file(&rig.data, missing).expect("spilled text file");
    std::fs::remove_file(&path).expect("delete spilled text");

    assert_turn_refused(&rig, &host, id, missing).await;
    host.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn non_utf8_text_blob_refuses_the_turn_without_sending() {
    let rig = rig();
    let mut stored = spilled_text().into_bytes();
    stored[100] = 0xff;
    assert!(std::str::from_utf8(&stored).is_err());
    let blob = BlobId::from_bytes(&stored);
    let length = blob_len(&stored);
    let id = seed_stored_blob(&rig.data, &rig.workspace, &stored, |digest| {
        vec![JournalPart::TextBlob {
            blob: digest.into(),
            bytes: length,
        }]
    })
    .await;

    let host = start_host(&rig).await;
    assert_turn_refused(&rig, &host, id, blob).await;
    host.shutdown(Duration::from_secs(1)).await;
}
