//! Reopening a stored session through a real host: the settings its journal
//! records chose reach the client view, and a journal without such records
//! leaves the host defaults in place. The journals are written the way the
//! store writes them and read back by a host that never saw the session.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at the failed boundary"
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::time::Duration;

use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    ApprovalMode, ClientId, Config, ConfigProduct, Entry, EntryId, EntryKind, Family, JournalPart,
    Mode, ModelRoute, PageReq, Product as JournalProduct, Record, SessionId, ThinkingLevel, View,
    Workspace,
};
use dal_store::Store;

const CLOSE: Duration = Duration::from_secs(5);

struct Rig {
    _temp: tempfile::TempDir,
    host: Host,
    store: Store,
    workspace: Workspace,
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero entry id"))
}

/// Entry `id`, chained under the entry before it.
fn entry(id: u64, kind: EntryKind) -> Entry {
    Entry {
        id: entry_id(id),
        parent: (id > 1).then(|| entry_id(id - 1)),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind,
    }
}

/// The first entry of every fixture journal.
fn user() -> Record {
    Record::User(entry(
        1,
        EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "earlier question".into(),
            }],
        },
    ))
}

fn name_record(name: Option<&str>) -> Record {
    Record::Name {
        at: jiff::Timestamp::UNIX_EPOCH,
        name: name.map(Into::into),
    }
}

fn route(model: &str, provider: Option<&str>) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: provider.map(Into::into),
    }
}

async fn rig() -> Rig {
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
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some("")).expect("configuration");
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions: Vec::new(),
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
    Rig {
        _temp: temp,
        host,
        store,
        workspace,
    }
}

/// Stores `records` as one session's journal, then closes it.
async fn seed(rig: &Rig, id: SessionId, records: Vec<Record>) {
    let mut journal = rig.store.create_session(id);
    journal.append(records).await.expect("seed the journal");
    journal.close().await.expect("close the seeded journal");
}

/// The view a client gets from the stored session `id`, reopened.
async fn reopen(rig: &Rig, id: SessionId) -> View {
    let agent = rig
        .host
        .open(
            SessionRef::Resume {
                key: id.to_string().into(),
                workspace: rig.workspace.clone(),
            },
            ClientId::new("settings-reopen"),
        )
        .await
        .expect("the stored session opens");
    agent.view(PageReq::default()).expect("view")
}

/// The view of a session this host just created: the host defaults.
async fn fresh(rig: &Rig) -> View {
    let agent = rig
        .host
        .open(
            SessionRef::Ephemeral {
                workspace: rig.workspace.clone(),
            },
            ClientId::new("settings-reopen"),
        )
        .await
        .expect("a fresh session opens");
    agent.view(PageReq::default()).expect("view")
}

#[tokio::test]
async fn a_reopened_session_shows_every_setting_its_journal_chose() {
    let rig = rig().await;
    let defaults = fresh(&rig).await.settings;
    // Every journaled value differs from the host default, so a view that
    // kept the default would not pass.
    assert_ne!(defaults.thinking, ThinkingLevel::High);
    assert_ne!(defaults.approval, ApprovalMode::All);
    assert_ne!(defaults.mode, Mode::EvalOnly);
    let chosen = route("gpt-4o", Some("acme"));
    let session = SessionId::new_v7();
    seed(
        &rig,
        session,
        vec![
            user(),
            Record::Model(entry(
                2,
                EntryKind::Model {
                    route: chosen.clone(),
                },
            )),
            Record::Thinking(entry(
                3,
                EntryKind::Thinking {
                    level: ThinkingLevel::High,
                },
            )),
            Record::Approval(entry(
                4,
                EntryKind::Approval {
                    mode: ApprovalMode::All,
                },
            )),
            Record::Mode(entry(
                5,
                EntryKind::Mode {
                    mode: Mode::EvalOnly,
                },
            )),
            name_record(Some("restored")),
        ],
    )
    .await;

    let view = reopen(&rig, session).await;
    assert_eq!(
        view.settings.model,
        Some(chosen),
        "the journaled route returns with its provider"
    );
    assert_eq!(view.settings.thinking, ThinkingLevel::High);
    assert_eq!(view.settings.approval, ApprovalMode::All);
    assert_eq!(view.settings.mode, Mode::EvalOnly);
    assert_eq!(view.settings.name.as_deref(), Some("restored"));
    assert_eq!(
        view.session.name.as_deref(),
        Some("restored"),
        "the session row carries the restored name"
    );
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn the_last_record_of_a_kind_wins_and_a_cleared_name_stays_cleared() {
    let rig = rig().await;
    let session = SessionId::new_v7();
    let last = route("gpt-4o", None);
    seed(
        &rig,
        session,
        vec![
            user(),
            Record::Model(entry(
                2,
                EntryKind::Model {
                    route: route("gpt-4o", Some("acme")),
                },
            )),
            Record::Model(entry(
                3,
                EntryKind::Model {
                    route: last.clone(),
                },
            )),
            Record::Thinking(entry(
                4,
                EntryKind::Thinking {
                    level: ThinkingLevel::High,
                },
            )),
            Record::Thinking(entry(
                5,
                EntryKind::Thinking {
                    level: ThinkingLevel::Low,
                },
            )),
            name_record(Some("first")),
            name_record(None),
        ],
    )
    .await;

    let view = reopen(&rig, session).await;
    assert_eq!(
        view.settings.model,
        Some(last),
        "the later route wins, here a legacy route without a provider"
    );
    assert_eq!(view.settings.thinking, ThinkingLevel::Low);
    assert_eq!(view.settings.name, None);
    let _ = rig.host.shutdown(CLOSE).await;
}

#[tokio::test]
async fn a_journal_without_setting_records_shows_the_host_defaults() {
    let rig = rig().await;
    let defaults = fresh(&rig).await.settings;
    let session = SessionId::new_v7();
    seed(&rig, session, vec![user()]).await;

    let view = reopen(&rig, session).await;
    assert_eq!(
        view.settings, defaults,
        "a session that chose nothing shows what a new session shows"
    );
    assert_eq!(view.session.name, None);
    let _ = rig.host.shutdown(CLOSE).await;
}

/// A session created with a name journals it before the first view, so the
/// first view already carries it: the exit line a client prints reads it there.
#[tokio::test]
async fn a_new_named_session_shows_its_name_in_its_first_view() {
    let rig = rig().await;
    let agent = rig
        .host
        .open(
            SessionRef::New {
                workspace: rig.workspace.clone(),
                name: Some("alpha".into()),
            },
            ClientId::new("settings-reopen"),
        )
        .await
        .expect("the named session opens");
    let view = agent.view(PageReq::default()).expect("view");
    assert_eq!(view.session.name.as_deref(), Some("alpha"));
    assert_eq!(view.settings.name.as_deref(), Some("alpha"));
    let _ = rig.host.shutdown(CLOSE).await;
}
