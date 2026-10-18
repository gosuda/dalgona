//! Session setup selects durable route identity before configuration fallback.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use dal_agent::ext::{BoxFuture, ExtensionBuilder, HookError, SessionCx, SessionInstalls};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    AssistantRoute, AssistantStop, ClientId, Config, ConfigProduct, Entry, EntryId, EntryKind,
    Family, JournalPart, ModelRoute, Product as JournalProduct, Record, ServiceSet, SessionId,
    Usage, Workspace,
};
use dal_store::Store;
use tokio::sync::Mutex;

const CONFIGURED_MODEL: &str = "openai/gpt-6-luna";
const RECORDED_MODEL: &str = "dalgon/normal";

#[derive(Debug, PartialEq)]
struct SetupRoute {
    model: Option<ModelRoute>,
    model_id: Box<str>,
}

fn entry_id(id: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(id).expect("nonzero entry id"))
}

fn assistant(id: u64, route: Option<AssistantRoute>) -> Record {
    Record::Assistant(Entry {
        id: entry_id(id),
        parent: Some(entry_id(id - 1)),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::Assistant {
            api: Family::Chat,
            model: RECORDED_MODEL.into(),
            route,
            content: Vec::new(),
            usage: Usage {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 1,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
                premium_requests: None,
            },
            stop: AssistantStop::Done,
            replay: None,
        },
    })
}

async fn resumed_setup(records: Vec<Record>) -> SetupRoute {
    let temp = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .expect("temporary store task")
        .expect("temporary store");
    let data = temp.path().join("data");
    let directory = temp.path().join("workspace");
    tokio::fs::create_dir_all(&data)
        .await
        .expect("data directory");
    tokio::fs::create_dir_all(&directory)
        .await
        .expect("workspace directory");
    let workspace = Workspace::new(directory.clone()).expect("workspace");
    let session = SessionId::new_v7();
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![Record::User(Entry {
            id: entry_id(1),
            parent: None,
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::User {
                parts: vec![JournalPart::Text {
                    text: "question".into(),
                }],
            },
        })])
        .await
        .expect("seed user");
    journal.append(records).await.expect("seed route history");
    journal.close().await.expect("close seeded journal");
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some(&format!("model = {CONFIGURED_MODEL:?}\n")),
    )
    .expect("configuration");
    let seen = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&seen);
    let extension = ExtensionBuilder::new("capture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension builder")
        .session_setup(
            move |cx: SessionCx| -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
                let captured = Arc::clone(&captured);
                Box::pin(async move {
                    *captured.lock().await = Some(SetupRoute {
                        model: cx.model,
                        model_id: cx.model_id,
                    });
                    Ok(SessionInstalls::default())
                })
            },
        )
        .build()
        .expect("setup extension");
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: vec![extension],
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::new(),
            cwd: directory,
            sandbox_helper: None,
        },
    )
    .await
    .expect("host");
    let _agent = host
        .open(
            SessionRef::Resume {
                workspace,
                key: session.to_string().into(),
            },
            ClientId::new("setup-route"),
        )
        .await
        .expect("resume session");
    host.close(session).await.expect("close session");
    let route = seen.lock().await.take().expect("setup was called");
    tokio::task::spawn_blocking(move || temp.close())
        .await
        .expect("temporary store cleanup task")
        .expect("temporary store cleanup");
    route
}

#[tokio::test]
async fn setup_uses_typed_assistant_route_before_configuration() {
    for (provenance, expected) in [
        (
            AssistantRoute::Api(Family::Chat),
            ModelRoute::Api {
                family: Family::Chat,
                model: RECORDED_MODEL.into(),
            },
        ),
        (
            AssistantRoute::Synthetic,
            ModelRoute::Synthetic {
                id: RECORDED_MODEL.into(),
            },
        ),
        (
            AssistantRoute::Harness,
            ModelRoute::Harness {
                id: RECORDED_MODEL.into(),
            },
        ),
    ] {
        assert_eq!(
            resumed_setup(vec![assistant(2, Some(provenance))]).await,
            SetupRoute {
                model: Some(expected),
                model_id: RECORDED_MODEL.into()
            },
        );
    }
}

#[tokio::test]
async fn setup_keeps_explicit_model_selection_ahead_of_assistant_route() {
    let selected = ModelRoute::Harness {
        id: "dalgon/eval-only".into(),
    };
    let records = vec![
        Record::Model(Entry {
            id: entry_id(2),
            parent: Some(entry_id(1)),
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Model {
                route: selected.clone(),
            },
        }),
        assistant(3, Some(AssistantRoute::Synthetic)),
    ];
    assert_eq!(
        resumed_setup(records).await,
        SetupRoute {
            model: Some(selected),
            model_id: "dalgon/eval-only".into()
        },
    );
}

#[tokio::test]
async fn setup_does_not_resurrect_route_before_latest_unknown_assistant() {
    assert_eq!(
        resumed_setup(vec![
            assistant(2, Some(AssistantRoute::Synthetic)),
            assistant(3, None),
        ])
        .await,
        SetupRoute {
            model: Some(ModelRoute::from_id(CONFIGURED_MODEL)),
            model_id: ModelRoute::from_id(CONFIGURED_MODEL).id().into(),
        },
    );
}

#[tokio::test]
async fn setup_keeps_recorded_route_after_compaction() {
    let records = vec![
        assistant(2, Some(AssistantRoute::Synthetic)),
        Record::Compaction(Entry {
            id: entry_id(3),
            parent: Some(entry_id(2)),
            at: jiff::Timestamp::UNIX_EPOCH,
            kind: EntryKind::Compaction {
                summary: Some("retained summary".into()),
                first_kept: None,
                tokens_before: 10,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        }),
    ];
    assert_eq!(
        resumed_setup(records).await,
        SetupRoute {
            model: Some(ModelRoute::Synthetic {
                id: RECORDED_MODEL.into()
            }),
            model_id: RECORDED_MODEL.into(),
        },
    );
}
