//! A provider 413 compacts the history and retries the turn instead of failing it.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, ExtensionBuilder, Services,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect, JournalPart, Part,
    Product as JournalProduct, Record, ServiceSet, SessionId, Stop, UpdateKind, Workspace,
};
use dal_store::Store;

/// One 413 answer, then one complete reply for the retried request.
const FIXTURE: &str = concat!(
    "{\"kind\":\"fail\",\"message\":\"Request Entity Too Large\",\"status\":413,\"family\":\"openai_chat\"}\n",
    "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"recovered\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n",
);

/// Commits a text summary for every span and counts the runs.
struct CountingCompactor(Arc<AtomicUsize>);

impl Compactor for CountingCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let span = input.span;
        Box::pin(async move { Ok(Some(Compaction::text(span, "compacted history", None))) })
    }
}

fn entry(id: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(id).expect("entry id is non-zero"))
}

fn user(id: u64, parent: Option<EntryId>, text: String) -> Record {
    Record::User(Entry {
        id: entry(id),
        parent,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

#[tokio::test]
async fn http_413_compacts_and_retries_the_turn() {
    let temp = tempfile::tempdir().expect("temp directory");
    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let session = SessionId::new_v7();
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user(1, None, "old prefix".into())])
        .await
        .expect("seed first user");
    journal
        .append(vec![user(2, Some(entry(1)), "retained".repeat(20_000))])
        .await
        .expect("seed second user");
    journal.close().await.expect("close seeded session");

    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, FIXTURE).expect("write provider fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&config_text)).expect("config");
    let runs = Arc::new(AtomicUsize::new(0));
    let extension = ExtensionBuilder::new("count", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .compactor("count", Arc::new(CountingCompactor(Arc::clone(&runs))))
        .build()
        .expect("count extension");
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
            ClientId::new("request-too-large"),
        )
        .await
        .expect("resume seeded session");
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "please continue".into(),
            }],
        })
        .await
        .expect("submit prompt");

    let stop = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(delivery) = subscription.next().await {
            if let dal_agent::Delivery::Update(update) = delivery
                && let UpdateKind::TurnEnded { stop, .. } = update.kind
            {
                return stop;
            }
        }
        panic!("subscription closed before the turn ended");
    })
    .await
    .expect("turn ends");

    assert_eq!(stop, Stop::EndTurn, "a 413 must not fail the turn");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "one compaction pass ran");
    host.close(session).await.expect("close session");
}
