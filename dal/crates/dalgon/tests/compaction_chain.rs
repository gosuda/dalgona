//! A bundled battery's compactor runs before dal's text-summary fallback.
//!
//! The chain is built by a real `Host` from the assembled dal product, so the
//! origin ordering that puts built-in extensions ahead of bundled ones is part
//! of what the test exercises.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, ExtensionBuilder, Services,
};
use dal_agent::{Env, Host, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect, JournalPart,
    Origin, PageReq, Part, Product as JournalProduct, Record, ServiceSet, SessionId, Workspace,
};
use dal_store::Store;
use dalgon::BuildCx;

/// One scripted turn that streams `text`, ends the turn, and reports ten input tokens.
fn events_step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

/// The turn the remote compactor takes: this model has no native compaction
/// endpoint, so the request runs as a plain turn and its result is refused.
const REMOTE_TURN: &str = "remote";

/// What the summary fallback writes when it gets its turn.
const FALLBACK_SUMMARY: &str = "scripted summary";

const BATTERY_SUMMARY: &str = "battery summary";

/// Commits a fixed text over the whole covered span.
struct Commits;

impl Compactor for Commits {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: std::sync::Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        let span = input.covered_span();
        Box::pin(std::future::ready(Ok(Some(Compaction::text(
            span,
            BATTERY_SUMMARY,
            None,
        )))))
    }
}

fn entry(id: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(id).expect("entry id is nonzero"))
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

/// The summary text of the first compaction entry in the session, once one exists.
fn compaction_summary(agent: &dal_agent::Agent) -> Option<Box<str>> {
    let view = agent.view(PageReq::default()).expect("session view");
    view.entries
        .items
        .into_iter()
        .find_map(|item| match item.kind {
            EntryKind::Compaction { summary, .. } => summary,
            _ => None,
        })
}

/// A resumed session with two seeded entries, a host running the dal product
/// plus a bundled battery whose compactor commits, and a scripted provider.
struct Fixture {
    _temp: tempfile::TempDir,
    host: Host,
    agent: dal_agent::Agent,
    session: SessionId,
}

/// Seeds a session where a compaction covers the large first entry and keeps the second.
async fn seed_session(store: &Store, session: SessionId) {
    let mut journal = store.create_session(session);
    journal
        .append(vec![user(1, None, "old prefix ".repeat(2_000))])
        .await
        .expect("seed first user");
    journal
        .append(vec![user(2, Some(entry(1)), "retained".repeat(20_000))])
        .await
        .expect("seed second user");
    journal.close().await.expect("close seeded session");
}

async fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("temp directory");
    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let session = SessionId::new_v7();
    seed_session(
        &Store::new(data.clone(), workspace.clone(), JournalProduct::Dal),
        session,
    )
    .await;

    let script = data.join("script.jsonl");
    let steps = format!(
        "{}{}{}",
        events_step("warm"),
        events_step(REMOTE_TURN),
        events_step(FALLBACK_SUMMARY)
    );
    std::fs::write(&script, steps).expect("write provider script");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        script.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&config_text)).expect("config");
    let cx = BuildCx {
        data_root: data,
        config: &config,
    };
    let mut parts = dalgon::parts(&cx).expect("dal parts");
    let battery = ExtensionBuilder::new("battery", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .with_origin(Origin::Bundled, None)
        .compactor("battery", std::sync::Arc::new(Commits))
        .build()
        .expect("battery extension");
    parts.batteries.push(battery);
    let product = dalgon::assemble(&cx, parts).expect("assembled product");
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
            ClientId::new("compaction-chain"),
        )
        .await
        .expect("resume seeded session");
    Fixture {
        _temp: temp,
        host,
        agent,
        session,
    }
}

/// Runs one prompt turn to its end.
async fn warm_turn(agent: &dal_agent::Agent, subscription: &mut dal_agent::Subscription) {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "warm".into(),
            }],
        })
        .await
        .expect("submit warm prompt");
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

/// Waits for the notice that ends a compaction, whether it committed or was
/// rejected, and returns its text.
async fn compaction_notice(subscription: &mut dal_agent::Subscription) -> Box<str> {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(delivery) = subscription.next().await {
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            if let dal_core::UpdateKind::Notice(notice) = &update.kind
                && notice.kind.as_ref() == "compaction_ended"
            {
                return Some(notice.text.clone());
            }
        }
        None
    })
    .await
    .expect("compaction ends in time")
    .expect("the subscription stays open until the compaction ends")
}

#[tokio::test]
async fn bundled_battery_compactor_commits_before_the_summary_fallback() {
    let Fixture {
        _temp,
        host,
        agent,
        session,
    } = fixture().await;
    let mut subscription = agent.subscribe(None).expect("subscribe");
    warm_turn(&agent, &mut subscription).await;
    agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");

    let ended = compaction_notice(&mut subscription).await;
    assert!(
        ended.starts_with("Context compacted by battery:"),
        "the battery must run before the text-summary fallback, which writes `{FALLBACK_SUMMARY}`: {ended}"
    );
    assert_eq!(
        compaction_summary(&agent).as_deref(),
        Some(BATTERY_SUMMARY),
        "the journal holds the battery's replacement"
    );
    host.close(session).await.expect("close session");
}
