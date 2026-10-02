//! The session driver supplies catalog and retained-context inputs to compactors.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, ExtensionBuilder, ImageProfile,
    Services,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect, JournalPart, Part,
    Product as JournalProduct, Record, ServiceSet, SessionId, Workspace,
};
use dal_store::Store;

const FIXTURE: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"warm\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

#[derive(Clone, Debug, PartialEq)]
struct Captured {
    image_profile: Option<ImageProfile>,
    images_elsewhere: usize,
    carried: Option<Box<str>>,
    first_kept: Option<EntryId>,
    covered_span: (EntryId, EntryId),
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
            carried: input.carried.clone(),
            first_kept: input.first_kept,
            covered_span: input.span,
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
    tokio::time::timeout(Duration::from_secs(10), async {
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
            vec![JournalPart::ImageBlob {
                mime: "image/png".into(),
                blob: image_id.to_string().into(),
                bytes: u64::try_from(image.len()).expect("image length fits"),
            }],
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
                first_kept: Some(entry(2)),
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
    let temp = tempfile::tempdir().expect("temp directory");
    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let session = SessionId::new_v7();
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    let image = b"retained image".to_vec();
    let image_id = dal_core::BlobId::from_bytes(&image);
    let mut records = seed_records(&image, image_id);
    let rest = records.split_off(1);
    journal.append(records).await.expect("seed first user");
    journal.put_blob(image).expect("write retained image");
    journal.append(rest).await.expect("seed session");
    journal.close().await.expect("close seeded session");

    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("write provider fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&config_text)).expect("config");
    let seen = Arc::new(Mutex::new(None));
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .compactor("capture", Arc::new(CaptureCompactor(Arc::clone(&seen))))
        .build()
        .expect("capture extension");
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
    (temp, agent, host, seen, session)
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
    assert_eq!(captured.image_profile, Some(ImageProfile::openai()));
    assert_eq!(captured.images_elsewhere, 1);
    assert_eq!(captured.carried.as_deref(), Some("carried history summary"));
    assert_eq!(captured.first_kept, Some(entry(2)));
    assert_eq!(captured.covered_span, (entry(1), entry(1)));
    host.close(session).await.expect("close session");
}
