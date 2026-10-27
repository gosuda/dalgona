//! The session driver supplies catalog and retained-context inputs to compactors.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, EventStream, Extension,
    ExtensionBuilder, ImageProfile, ModelCx, ModelError, ModelHandler, ModelRecord, Services,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    AssistantPart, Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Entry, EntryId,
    EntryKind, Expect, JournalPart, ModelId, ModelRequest, Part, Product as JournalProduct, Record,
    ServiceSet, SessionId, ThinkingLevel, Usage, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use dal_store::Store;

const FIXTURE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"warm\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// Stored length of the retained blob image, `b"retained image"`.
const RETAINED_IMAGE_BYTES: u64 = 14;

/// The inline image `"hello"` in base64: five decoded bytes behind padding.
const INLINE_IMAGE_BASE64: &str = "aGVsbG8=";
const INLINE_IMAGE_BYTES: u64 = 5;

#[derive(Clone, Debug, PartialEq)]
struct Captured {
    image_profile: Option<ImageProfile>,
    images_elsewhere: usize,
    image_bytes_elsewhere: u64,
    carried: Option<Box<str>>,
    first_kept: Option<EntryId>,
    covered_span: (EntryId, EntryId),
    covered: Vec<(EntryId, Option<Box<str>>)>,
    context_items: usize,
    estimated_tokens: Vec<u64>,
    instructions: String,
}

struct CaptureCompactor(Arc<Mutex<Option<Captured>>>);

impl Compactor for CaptureCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Captured {
            image_profile: input.image_profile,
            images_elsewhere: input.images_elsewhere,
            image_bytes_elsewhere: input.image_bytes_elsewhere,
            carried: input.carried.clone(),
            first_kept: input.first_kept,
            covered_span: input.span,
            covered: input
                .covered
                .iter()
                .map(|covered| (covered.entry, covered.note.clone()))
                .collect(),
            estimated_tokens: input
                .covered
                .iter()
                .map(|covered| covered.estimated_tokens)
                .collect(),
            context_items: input.covered_context().len(),
            instructions: input.instructions.to_string(),
        });
        Box::pin(async { Ok(None) })
    }
}

fn entry(id: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(id).expect("entry id is nonzero"))
}

fn user(id: u64, parent: Option<EntryId>, parts: Vec<JournalPart>) -> Record {
    Record::User(Entry {
        id: entry(id),
        parent,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User { parts },
    })
}

async fn turn_ended(subscription: &mut dal_agent::Subscription) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(delivery) = subscription.next().await {
            if matches!(
                delivery,
                dal_agent::Delivery::Update(update)
                    if matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. })
            ) {
                return;
            }
        }
    })
    .await
    .expect("warm turn ends");
}

/// Seeds the journal records a manual compaction walks.
fn seed_records(image: &[u8], image_id: dal_core::BlobId) -> Vec<Record> {
    vec![
        user(
            1,
            None,
            vec![JournalPart::Text {
                text: "old prefix".into(),
            }],
        ),
        user(
            2,
            Some(entry(1)),
            vec![JournalPart::Text {
                text: "retained".repeat(20_000).into(),
            }],
        ),
        user(
            3,
            Some(entry(2)),
            vec![
                JournalPart::ImageBlob {
                    mime: "image/png".into(),
                    blob: image_id.to_string().into(),
                    bytes: u64::try_from(image.len()).expect("image length fits"),
                },
                JournalPart::Image {
                    mime: "image/png".into(),
                    base64: INLINE_IMAGE_BASE64.into(),
                },
            ],
        ),
        user(
            4,
            Some(entry(3)),
            vec![JournalPart::Text {
                text: "latest".into(),
            }],
        ),
        Record::Compaction(Entry {
            id: entry(5),
            parent: Some(entry(4)),
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Compaction {
                summary: Some("carried history summary".into()),
                first_kept: Some(entry(1)),
                tokens_before: 40_000,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        }),
    ]
}

/// Builds the seeded session, scripted fixture, and host the compaction test drives.
async fn compact_fixture() -> (
    tempfile::TempDir,
    dal_agent::Agent,
    Host,
    Arc<Mutex<Option<Captured>>>,
    SessionId,
) {
    let image = b"retained image".to_vec();
    let image_id = dal_core::BlobId::from_bytes(&image);
    compact_fixture_with(seed_records(&image, image_id), Some(image)).await
}

/// Builds a session seeded with `records`, and stores `blob` after the first record.
async fn compact_fixture_with(
    records: Vec<Record>,
    blob: Option<Vec<u8>>,
) -> (
    tempfile::TempDir,
    dal_agent::Agent,
    Host,
    Arc<Mutex<Option<Captured>>>,
    SessionId,
) {
    let seen = Arc::new(Mutex::new(None));
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .compactor("capture", Arc::new(CaptureCompactor(Arc::clone(&seen))))
        .build()
        .expect("capture extension");
    let (temp, agent, host, session) =
        seeded_host(records, blob, "openai/gpt-6-luna", extension).await;
    (temp, agent, host, seen, session)
}

/// Seeds `records`, stores `blob` after the first record, and resumes the session
/// on a host whose default model is `model` and whose only extension is `extension`.
async fn seeded_host(
    records: Vec<Record>,
    blob: Option<Vec<u8>>,
    model: &str,
    extension: Extension,
) -> (tempfile::TempDir, dal_agent::Agent, Host, SessionId) {
    let temp = tempfile::tempdir().expect("temp directory");
    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let session = SessionId::new_v7();
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    let mut records = records;
    let rest = records.split_off(1);
    journal.append(records).await.expect("seed first user");
    if let Some(blob) = blob {
        journal.put_blob(blob).expect("write retained image");
    }
    journal.append(rest).await.expect("seed session");
    journal.close().await.expect("close seeded session");

    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("write provider fixture");
    let fixture = fixture.to_string_lossy().replace('\\', "\\\\");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"{model}\"\n\n[providers.scripted]\nfixture = \"{fixture}\"\n"
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&config_text)).expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: std::collections::BTreeMap::from([(
            std::ffi::OsString::from("OPENAI_API_KEY"),
            std::ffi::OsString::from("sk-test"),
        )]),
        cwd: workspace_dir,
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let agent = host
        .open(
            SessionRef::Resume {
                workspace,
                key: session.to_string().into(),
            },
            ClientId::new("compact-input"),
        )
        .await
        .expect("resume seeded session");
    (temp, agent, host, session)
}

/// Runs one warm turn and a manual compaction, then returns what the compactor saw.
async fn warm_then_compact(
    agent: &dal_agent::Agent,
    seen: &Arc<Mutex<Option<Captured>>>,
) -> Captured {
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "warm".into(),
            }],
        })
        .await
        .expect("submit warm prompt");
    turn_ended(&mut subscription).await;
    let compact_reply = agent.submit(Command::Compact { focus: None }).await;
    assert!(
        compact_reply.is_ok(),
        "manual compact reply: {compact_reply:?}; captured={:?}",
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    );

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(captured) = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                return captured;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("compactor receives selected context")
}

#[tokio::test]
async fn driver_passes_catalog_profile_retained_images_and_carried_summary() {
    let (_temp, agent, host, seen, session) = compact_fixture().await;
    assert_eq!(
        agent
            .view(dal_core::PageReq::default())
            .expect("initial view")
            .session
            .id,
        session
    );
    let captured = warm_then_compact(&agent, &seen).await;
    assert_eq!(captured.image_profile, Some(ImageProfile::openai()));
    assert_eq!(captured.images_elsewhere, 2);
    assert_eq!(
        captured.image_bytes_elsewhere,
        RETAINED_IMAGE_BYTES + INLINE_IMAGE_BYTES
    );
    assert_eq!(captured.carried.as_deref(), Some("carried history summary"));
    assert_eq!(captured.first_kept, Some(entry(2)));
    assert_eq!(captured.covered_span, (entry(1), entry(1)));
    assert_eq!(captured.covered, vec![(entry(1), None)]);
    assert_eq!(captured.context_items, 1);
    host.close(session).await.expect("close session");
}

#[tokio::test]
async fn compact_focus_reaches_compactor() {
    let (_temp, agent, host, seen, session) = compact_fixture().await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "warm".into(),
            }],
        })
        .await
        .expect("submit warm prompt");
    turn_ended(&mut subscription).await;
    let compact_reply = agent
        .submit(Command::Compact {
            focus: Some("auth flow".into()),
        })
        .await;
    assert!(
        compact_reply.is_ok(),
        "manual compact reply: {compact_reply:?}; captured={:?}",
        *seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    );
    let captured = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(captured) = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                return captured;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("compactor receives selected context");
    assert_eq!(
        captured.instructions, "auth flow",
        "stored compact focus must reach CompactInput.instructions"
    );
    host.close(session).await.expect("close session");
}

#[tokio::test]
async fn driver_hands_reminders_to_compactors_as_notes_outside_the_context() {
    let records = vec![
        user(
            1,
            None,
            vec![JournalPart::Text {
                text: "old prefix".into(),
            }],
        ),
        Record::Reminder(Entry {
            id: entry(2),
            parent: Some(entry(1)),
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Reminder {
                source: "rule:gate".into(),
                text: "Respect the gate rule.".into(),
            },
        }),
        user(
            3,
            Some(entry(2)),
            vec![JournalPart::Text {
                text: "retained".repeat(20_000).into(),
            }],
        ),
    ];
    let (_temp, agent, host, seen, session) = compact_fixture_with(records, None).await;
    let captured = warm_then_compact(&agent, &seen).await;
    assert_eq!(captured.first_kept, Some(entry(3)));
    assert_eq!(captured.covered_span, (entry(1), entry(2)));
    assert_eq!(
        captured.covered,
        vec![
            (entry(1), None),
            (entry(2), Some("Respect the gate rule.".into())),
        ]
    );
    assert_eq!(
        captured.context_items, 1,
        "the reminder is not model context"
    );
    host.close(session).await.expect("close session");
}

#[tokio::test]
async fn driver_hands_before_turn_text_to_compactors_as_user_context() {
    let records = vec![
        user(
            1,
            None,
            vec![JournalPart::Text {
                text: "old prefix".into(),
            }],
        ),
        Record::Reminder(Entry {
            id: entry(2),
            parent: Some(entry(1)),
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Reminder {
                source: dal_core::BEFORE_TURN_SOURCE.into(),
                text: "old prefix".into(),
            },
        }),
        user(
            3,
            Some(entry(2)),
            vec![JournalPart::Text {
                text: "retained".repeat(20_000).into(),
            }],
        ),
    ];
    let (_temp, agent, host, seen, session) = compact_fixture_with(records, None).await;
    let captured = warm_then_compact(&agent, &seen).await;
    assert_eq!(captured.covered_span, (entry(1), entry(2)));
    assert_eq!(captured.covered, vec![(entry(1), None), (entry(2), None)]);
    assert_eq!(captured.context_items, 2, "the hook text is user context");
    assert_ne!(captured.estimated_tokens[1], 0);
    assert_eq!(
        captured.estimated_tokens[1], captured.estimated_tokens[0],
        "the hook text weighs as the same text in a user entry"
    );
    host.close(session).await.expect("close session");
}

/// Tokens the seeded compaction entry bills for its replacement page.
const PAGE_TOKENS: u64 = 1_000;

/// The recompacting compactor's replacement; at the shared 3.5-characters-per-token
/// rate it costs `REPLACEMENT_TOKENS`.
const REPLACEMENT_CHARACTERS: usize = 1_400;
const REPLACEMENT_TOKENS: u64 = 400;

/// How long a recompaction step may take before the test fails.
const STEP: Duration = Duration::from_secs(15);

/// The registered model the recompaction tests run on.
const MODEL: &str = "acme/recorder";

fn replacement() -> String {
    "r".repeat(REPLACEMENT_CHARACTERS)
}

/// The covered entries the driver handed the compactor on its latest call,
/// with their estimates, and how many times it was called.
#[derive(Clone, Debug, Default)]
struct Covered {
    entries: Vec<(EntryId, u64)>,
    calls: usize,
}

impl Covered {
    fn ids(&self) -> Vec<EntryId> {
        self.entries.iter().map(|(id, _)| *id).collect()
    }

    fn total(&self) -> u64 {
        self.entries.iter().map(|(_, tokens)| *tokens).sum()
    }
}

/// Commits `replacement()` over whatever span it is given and records the
/// covered entries it was handed.
struct ReplaceCompactor(Arc<Mutex<Covered>>);

impl Compactor for ReplaceCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        let entries = input
            .covered
            .iter()
            .map(|covered| (covered.entry, covered.estimated_tokens))
            .collect();
        let mut seen = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.entries = entries;
        seen.calls += 1;
        drop(seen);
        let compaction = Compaction::text(input.span, replacement(), None);
        Box::pin(async move { Ok(Some(compaction)) })
    }
}

/// A registered model that answers `ok`, reports `input_tokens` of context, and
/// records every request it receives.
struct Recorder {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    input_tokens: u64,
}

impl ModelHandler for Recorder {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let usage = Usage {
            input_tokens: self.input_tokens,
            cached_input_tokens: 0,
            output_tokens: 5,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
            premium_requests: None,
        };
        let events: Vec<Result<StreamEvent, ProviderError>> = vec![
            Ok(StreamEvent::TextDelta { text: "ok".into() }),
            Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
            Ok(StreamEvent::Usage { usage }),
            Ok(StreamEvent::Stop {
                reason: StopReason::EndTurn,
            }),
        ];
        Box::pin(async move { Ok(EventStream::new(futures::stream::iter(events), || {})) })
    }
}

/// An inline text part of `bytes` bytes of `marker`, under the store's spill bound.
fn marked(marker: char, bytes: usize) -> JournalPart {
    JournalPart::Text {
        text: marker.to_string().repeat(bytes).into(),
    }
}

/// A compaction entry that replaces everything before `first_kept` with one
/// page of `page_bytes` bytes, billed at `PAGE_TOKENS`.
fn paged_compaction(id: u64, parent: u64, first_kept: u64, page_bytes: usize) -> Record {
    Record::Compaction(Entry {
        id: entry(id),
        parent: Some(entry(parent)),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::Compaction {
            summary: None,
            first_kept: Some(entry(first_kept)),
            tokens_before: 40_000,
            replay: None,
            usage: None,
            parts: vec![marked('p', page_bytes)],
            parts_tokens: PAGE_TOKENS,
        },
    })
}

/// A seeded session after one warm turn and one manual compaction.
struct Recompacted {
    _temp: tempfile::TempDir,
    agent: dal_agent::Agent,
    host: Host,
    session: SessionId,
    subscription: dal_agent::Subscription,
    covered: Covered,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    /// Kind of every notice up to and including the awaited one.
    notices: Vec<String>,
}

impl Recompacted {
    /// The summary of every compaction entry on the branch, oldest first.
    fn compactions(&self) -> Vec<Option<Box<str>>> {
        let view = self
            .agent
            .view(dal_core::PageReq::default())
            .expect("session view");
        view.entries
            .items
            .into_iter()
            .filter_map(|entry| match entry.kind {
                EntryKind::Compaction { summary, .. } => Some(summary),
                _ => None,
            })
            .collect()
    }

    /// How many requests the registered model received.
    fn request_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// The text of every part of the newest request, in order.
    fn last_request_texts(&self) -> Vec<String> {
        let requests = self
            .requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = requests.last().expect("the model received a request");
        let mut texts = Vec::new();
        for item in request.context.iter() {
            match item {
                ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => {
                    texts.extend(parts.iter().filter_map(|part| match part {
                        Part::Text { text } => Some(text.to_string()),
                        Part::Image { .. } | Part::Blob { .. } => None,
                    }));
                }
                ContextItem::Assistant { parts, .. } => {
                    texts.extend(parts.iter().filter_map(|part| match part {
                        AssistantPart::Text { text, .. } => Some(text.to_string()),
                        _ => None,
                    }));
                }
            }
        }
        texts
    }

    /// Submits one prompt and waits for its turn to end.
    async fn prompt(&mut self, text: &str) {
        self.agent
            .submit(Command::Prompt {
                expect: Expect::Idle,
                content: vec![Part::Text { text: text.into() }],
            })
            .await
            .expect("submit prompt");
        turn_ended(&mut self.subscription).await;
    }

    async fn close(self) {
        self.host.close(self.session).await.expect("close session");
    }
}

/// Reads notice kinds until one equals `kind`, and returns every kind seen,
/// that one last.
async fn notices_until(subscription: &mut dal_agent::Subscription, kind: &str) -> Vec<String> {
    tokio::time::timeout(STEP, async {
        let mut seen = Vec::new();
        while let Some(delivery) = subscription.next().await {
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            let dal_core::UpdateKind::Notice(notice) = &update.kind else {
                continue;
            };
            seen.push(notice.kind.to_string());
            if notice.kind.as_ref() == kind {
                break;
            }
        }
        seen
    })
    .await
    .expect("the awaited notice arrives in time")
}

/// Seeds `records`, runs one warm turn on a registered model that reports
/// `input_tokens` of context, compacts manually, and waits for a notice of
/// kind `until`.
async fn recompact(records: Vec<Record>, input_tokens: u64, until: &str) -> Recompacted {
    assert_eq!(
        dal_core::estimate_text_tokens(&replacement()),
        REPLACEMENT_TOKENS,
        "the replacement costs what the premises below assume"
    );
    let covered = Arc::new(Mutex::new(Covered::default()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let extension = ExtensionBuilder::new("recompact", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .compactor("replace", Arc::new(ReplaceCompactor(Arc::clone(&covered))))
        .model(ModelRecord {
            id: ModelId::parse(MODEL).expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: true,
                custom_grammar: false,
            },
            handler: Arc::new(Recorder {
                requests: Arc::clone(&requests),
                input_tokens,
            }),
            export: None,
        })
        .build()
        .expect("recompact extension");
    let (temp, agent, host, session) = seeded_host(records, None, MODEL, extension).await;
    let subscription = agent.subscribe(None).expect("subscribe");
    let mut run = Recompacted {
        _temp: temp,
        agent,
        host,
        session,
        subscription,
        covered: Covered::default(),
        requests,
        notices: Vec::new(),
    };
    run.prompt("warm").await;
    run.agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    run.notices = notices_until(&mut run.subscription, until).await;
    run.covered = covered
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    run
}

/// The seeded compaction's page and the entry it replaced no longer count as
/// context once a second compaction supersedes the page, so the replacement
/// shrinks it and is committed. Counting the covered entries alone, the old
/// accounting saw them pay less than the replacement and rejected it.
#[tokio::test]
async fn a_second_compaction_superseding_a_page_commits_and_replaces_it_in_the_next_request() {
    let records = vec![
        user(1, None, vec![marked('b', 100)]),
        user(2, Some(entry(1)), vec![marked('c', 100)]),
        paged_compaction(3, 2, 2, 3_000),
        user(4, Some(entry(3)), vec![marked('d', 100)]),
    ];
    let mut run = recompact(records, 40_000, "compaction_ended").await;
    assert_eq!(
        run.covered.ids(),
        [entry(1), entry(2), entry(4)],
        "the cut passes the compaction and still covers the path from the root"
    );
    assert_eq!(run.covered.calls, 1, "the compactor ran once");
    assert!(
        run.covered.total() < REPLACEMENT_TOKENS,
        "the covered entries alone do not pay for the replacement"
    );
    assert_eq!(
        run.compactions(),
        [None, Some(replacement().into_boxed_str())],
        "the second compaction is committed"
    );

    run.prompt("follow up").await;
    assert_eq!(
        run.last_request_texts(),
        [replacement().as_str(), "warm", "ok", "follow up"],
        "the next request carries the replacement and the kept exchange, and neither \
         the superseded page nor the entries before the cut"
    );
    run.close().await;
}

/// A cut that stops at the first compaction's own kept entry covers only what
/// that compaction already replaced. Nothing leaves the live context, so any
/// replacement would only add to it: the driver refuses before it runs a
/// compactor, and the journal keeps its one compaction.
///
/// The replaced entry alone is larger than the replacement, so counting it as
/// removed would make the replacement look shrinking, commit it, and fail the
/// journal assertion below.
#[tokio::test]
async fn a_second_compaction_that_covers_only_already_replaced_entries_is_rejected() {
    let replaced = marked('b', 14_000);
    let JournalPart::Text { text } = &replaced else {
        unreachable!("`marked` builds inline text");
    };
    assert!(
        dal_core::estimate_text_tokens(text) > REPLACEMENT_TOKENS,
        "counted as removed, the replaced entry alone would pay for the replacement"
    );
    let records = vec![
        user(1, None, vec![replaced]),
        user(2, Some(entry(1)), vec![marked('c', 14_000); 6]),
        paged_compaction(3, 2, 2, 100),
        user(4, Some(entry(3)), vec![marked('d', 100)]),
    ];
    let run = recompact(records, 15_000, "compact.manual_failed").await;
    assert_eq!(
        run.covered.calls, 0,
        "no compactor runs when nothing live precedes the cut"
    );
    assert_eq!(
        run.request_count(),
        1,
        "only the warm turn reached the model"
    );
    assert!(
        run.notices
            .iter()
            .any(|kind| kind == "compact.manual_failed"),
        "the manual compaction reports its failure: {:?}",
        run.notices
    );
    assert_eq!(
        run.compactions(),
        [None],
        "no compaction is added to the journal"
    );
    run.close().await;
}
