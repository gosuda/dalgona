//! Grant key, broker, coalescing, and persistence tests.

use dal_core::{Name, Origin, Service};

fn key(ext: &str, origin: Origin, services: &[Service]) -> GrantKey {
    GrantKey {
        extension: ext.parse::<Name>().expect("valid name"),
        origin,
        services: ServiceSet::from_names(services.iter().map(|service| service.as_str()))
            .expect("valid set"),
    }
}

#[test]
fn grant_key_is_sorted_exact_and_excludes_ask() {
    let full = [Service::Run, Service::Ask, Service::FsRead];
    let permuted = [Service::Ask, Service::FsRead, Service::Run];
    let mut without_ask: Vec<Service> = full
        .iter()
        .copied()
        .filter(|s| *s != Service::Ask)
        .collect();
    without_ask.sort();
    let a = key("focus", Origin::User, &without_ask);
    let b = key("focus", Origin::User, &permuted[1..]);
    assert_eq!(a, b);
    assert!(!a.services.contains(Service::Ask));
    assert_eq!(
        a.services.iter().collect::<Vec<_>>(),
        vec![Service::FsRead, Service::Run]
    );

    let changed_service = key("focus", Origin::User, &[Service::FsRead, Service::Net]);
    let changed_origin = key("focus", Origin::Bundled, &[Service::FsRead, Service::Run]);
    let changed_ext = key("other", Origin::User, &[Service::FsRead, Service::Run]);
    assert_ne!(changed_service, a);
    assert_ne!(changed_origin, a);
    assert_ne!(changed_ext, a);
}

use std::sync::Arc;
use std::time::Duration;

use dal_core::{Answer, ClientId, DenyReason, ServiceSet, Timestamp, TurnId};
use tokio_util::sync::CancellationToken;

use crate::Broker;
use crate::error::ServiceError;
use crate::ext::{Caller, CallerKind};

use super::{GrantKey, GrantStore, GrantStoreError};

fn test_caller(inject: ServiceSet) -> Caller {
    Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        CallerKind::Hook,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    )
}
fn test_mcp_caller(inject: ServiceSet) -> Caller {
    Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        CallerKind::Tool,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    )
}

fn open_store(dir: &tempfile::TempDir, timeout: Duration) -> (Arc<GrantStore>, Arc<Broker>) {
    let broker = Arc::new(Broker::new());
    let store = Arc::new(
        GrantStore::with_runtime(dir.path().to_path_buf(), timeout, broker.clone()).expect("store"),
    );
    (store, broker)
}

fn tui() -> ClientId {
    ClientId::new("tui")
}

#[tokio::test]
async fn grant_misses_coalesce_and_revoke_invalidates_late_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["fs.read", "run"]).expect("inject");
    let caller_a = test_caller(inject);
    let caller_b = test_caller(inject);
    let cancel = CancellationToken::new();
    let ext: Name = "focus".parse().expect("name");
    let (ra, rb, ()) = futures::join!(
        store.ensure(&caller_a, Service::Run, &cancel),
        store.ensure(&caller_b, Service::Run, &cancel),
        async {
            let id = loop {
                if let Some(req) = broker.open_requests().into_iter().next() {
                    break req.id;
                }
                tokio::task::yield_now().await;
            };
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            store.revoke(&ext).await.expect("revoke");
            broker.answer(id, Answer::Approve, tui()).expect("late");
        }
    );
    assert!(matches!(ra, Err(ServiceError::Cancelled)));
    assert!(matches!(rb, Err(ServiceError::Cancelled)));
    assert!(!dir.path().join("grants.toml").exists());
}

#[tokio::test]
async fn grant_approve_persists_exact_row_and_session_approve_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["fs.read", "run"]).expect("inject");
    let caller = test_caller(inject);
    let cancel = CancellationToken::new();
    let (grant, ()) = futures::join!(store.ensure(&caller, Service::Run, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        broker.answer(id, Answer::Approve, tui()).expect("approve");
    });
    let grant = grant.expect("granted");
    assert!(grant.persistent());
    let text = std::fs::read_to_string(dir.path().join("grants.toml")).expect("row");
    assert!(text.starts_with(
        "[[grant]]\next = \"focus\"\norigin = \"user\"\nservices = [\"fs.read\", \"run\"]\nby = \"tui\"\napproved_at = \""
    ));
    assert!(text.ends_with("Z\"\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.path().join("grants.toml"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    let store2 = Arc::new(
        GrantStore::with_runtime(
            dir.path().to_path_buf(),
            Duration::from_secs(30),
            broker.clone(),
        )
        .expect("reopen"),
    );
    let grant2 = store2
        .ensure(&caller, Service::Run, &cancel)
        .await
        .expect("cached");
    assert!(grant2.persistent());
    assert!(broker.open_requests().is_empty());
    let net_caller = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        ServiceSet::from_names(["net"]).expect("net"),
        CallerKind::Hook,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    );
    let (session_grant, ()) =
        futures::join!(store2.ensure(&net_caller, Service::Net, &cancel), async {
            let id = loop {
                if let Some(req) = broker.open_requests().into_iter().next() {
                    break req.id;
                }
                tokio::task::yield_now().await;
            };
            broker
                .answer(id, Answer::ApproveForSession, tui())
                .expect("session approve");
        });
    let session_grant = session_grant.expect("session");
    assert!(!session_grant.persistent());
    let after = std::fs::read_to_string(dir.path().join("grants.toml")).expect("row again");
    assert_eq!(text, after);
    let fresh_dir = tempfile::tempdir().expect("fresh");
    let fresh = GrantStore::with_runtime(
        fresh_dir.path().to_path_buf(),
        Duration::from_secs(30),
        broker.clone(),
    )
    .expect("fresh");
    let cancel2 = CancellationToken::new();
    let (missing, ()) = futures::join!(fresh.ensure(&net_caller, Service::Net, &cancel2), async {
        loop {
            if broker.open_requests().len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        cancel2.cancel();
    });
    assert!(matches!(
        missing,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));
    assert_eq!(broker.open_requests().len(), 1);
}

#[tokio::test]
async fn grant_denials_are_distinct() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["run"]).expect("inject");
    let caller = test_caller(inject);
    let cancel = CancellationToken::new();
    for answer in [Answer::Decline, Answer::Cancel] {
        let probe = answer.clone();
        let (outcome, ()) = futures::join!(store.ensure(&caller, Service::Run, &cancel), async {
            let id = loop {
                if let Some(req) = broker.open_requests().into_iter().next() {
                    break req.id;
                }
                tokio::task::yield_now().await;
            };
            broker.answer(id, answer, tui()).expect("answer");
        });
        match probe {
            Answer::Decline => assert!(matches!(outcome, Err(ServiceError::Declined))),
            Answer::Cancel => assert!(matches!(outcome, Err(ServiceError::Cancelled))),
            _ => panic!("unexpected probe"),
        }
    }
    let (quick, _) = open_store(&dir, Duration::from_millis(50));
    let slow = quick.ensure(&caller, Service::Run, &cancel).await;
    assert!(matches!(
        slow,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));
}

#[tokio::test]
async fn facade_grant_contains_list_revoke_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = GrantStore::new(dir.path().to_path_buf());
    let name: Name = "focus".parse().expect("name");
    let key = GrantKey {
        extension: name.clone(),
        origin: Origin::User,
        services: ServiceSet::from_names(["fs.read", "run"]).expect("set"),
    };
    assert!(!store.contains(&key).await.expect("contains"));
    assert!(store.list().await.expect("list").is_empty());
    let at = Timestamp::now();
    assert!(store.grant(key.clone(), tui(), at).await.expect("grant"));
    assert!(
        !store
            .grant(key.clone(), tui(), at)
            .await
            .expect("idempotent")
    );
    let empty = GrantKey {
        extension: name.clone(),
        origin: Origin::User,
        services: ServiceSet::EMPTY,
    };
    assert!(matches!(
        store.grant(empty, tui(), at).await,
        Err(GrantStoreError::Malformed { .. })
    ));
    assert!(store.contains(&key).await.expect("contains"));
    let listed = store.list().await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, key);
    assert_eq!(listed[0].by, tui());
    assert_eq!(listed[0].approved_at, at);
    assert_eq!(store.revoke(&name).await.expect("revoke"), 1);
    assert_eq!(store.revoke(&name).await.expect("revoke"), 0);
    assert!(store.list().await.expect("list").is_empty());
}

#[tokio::test]
async fn facade_malformed_file_fails_without_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("grants.toml"), "[[grant]]\next = oops\n").expect("write");
    let before = std::fs::read_to_string(dir.path().join("grants.toml")).expect("snapshot");
    let store = GrantStore::new(dir.path().to_path_buf());
    assert!(matches!(
        store.list().await,
        Err(GrantStoreError::Malformed { .. })
    ));
    let key = GrantKey {
        extension: "focus".parse().expect("name"),
        origin: Origin::User,
        services: ServiceSet::from_names(["run"]).expect("set"),
    };
    assert!(matches!(
        store.contains(&key).await,
        Err(GrantStoreError::Malformed { .. })
    ));
    assert!(matches!(
        store.grant(key, tui(), Timestamp::now()).await,
        Err(GrantStoreError::Malformed { .. })
    ));
    let name: Name = "focus".parse().expect("name");
    assert!(matches!(
        store.revoke(&name).await,
        Err(GrantStoreError::Malformed { .. })
    ));
    let after = std::fs::read_to_string(dir.path().join("grants.toml")).expect("snapshot");
    assert_eq!(before, after);
}

#[tokio::test]
async fn mcp_grants_persist_for_exact_declared_set_and_reask_after_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let caller = test_mcp_caller(ServiceSet::from_names(["mcp"]).expect("inject"));
    let cancel = CancellationToken::new();
    let (approved, ()) = futures::join!(
        store.ensure_declared_mcp(
            &caller,
            "set-a".into(),
            "search: URL https://mcp.example/search".into(),
            &cancel,
        ),
        async {
            let request = loop {
                if let Some(request) = broker.open_requests().into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            };
            let dal_core::Question::Grant { detail, .. } = &request.question else {
                panic!("MCP grant must ask a grant question");
            };
            assert_eq!(
                detail.as_deref(),
                Some("search: URL https://mcp.example/search")
            );
            broker
                .answer(request.id, Answer::Approve, tui())
                .expect("approve");
        }
    );
    assert!(approved.expect("approved").persistent());
    let text = std::fs::read_to_string(dir.path().join("grants.toml")).expect("persisted grant");
    assert!(text.contains("mcp_set = \"set-a\""));
    assert_eq!(
        store.list().await.expect("list")[0].mcp_set.as_deref(),
        Some("set-a")
    );
    assert!(
        store
            .ensure_declared_mcp(
                &caller,
                "set-a".into(),
                "search: URL https://mcp.example/search".into(),
                &cancel,
            )
            .await
            .expect("same declaration is cached")
            .persistent()
    );
    assert!(broker.open_requests().is_empty());

    let (changed, ()) = futures::join!(
        store.ensure_declared_mcp(
            &caller,
            "set-b".into(),
            "search: URL https://mcp.example/search".into(),
            &cancel,
        ),
        async {
            let request = loop {
                if let Some(request) = broker.open_requests().into_iter().next() {
                    break request;
                }
                tokio::task::yield_now().await;
            };
            broker
                .answer(request.id, Answer::ApproveForSession, tui())
                .expect("session approve");
        }
    );
    assert!(!changed.expect("changed set approved").persistent());
    assert!(broker.open_requests().is_empty());
}

#[tokio::test]
async fn mcp_grant_misses_coalesce_and_publish_request_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["mcp"]).expect("inject");
    let caller_a = test_mcp_caller(inject);
    let caller_b = test_mcp_caller(inject);
    let cancel = CancellationToken::new();
    let updates = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let published = Arc::clone(&updates);
    let grant_file = dir.path().join("grants.toml");
    store.set_request_update(Arc::new(move |update| {
        if matches!(&update, dal_core::UpdateKind::RequestResolved { .. }) {
            assert!(grant_file.exists());
        }
        published
            .try_lock()
            .expect("request update callbacks do not overlap")
            .push(update);
    }));

    let (first, second, ()) = futures::join!(
        store.ensure_declared_mcp(
            &caller_a,
            "set-a".into(),
            "search: URL https://mcp.example/search".into(),
            &cancel,
        ),
        store.ensure_declared_mcp(
            &caller_b,
            "set-a".into(),
            "search: URL https://mcp.example/search".into(),
            &cancel,
        ),
        async {
            let request = loop {
                let requests = broker.open_requests();
                if !requests.is_empty() {
                    assert_eq!(requests.len(), 1);
                    break requests.into_iter().next().expect("one request");
                }
                tokio::task::yield_now().await;
            };
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            assert_eq!(broker.open_requests().len(), 1);
            broker
                .answer(request.id, Answer::Approve, tui())
                .expect("approve");
        }
    );
    assert!(first.expect("first granted").persistent());
    assert!(second.expect("second granted").persistent());

    let updates = updates.lock().await;
    assert_eq!(updates.len(), 2);
    let dal_core::UpdateKind::RequestOpened(opened) = &updates[0] else {
        panic!("grant request must be published when opened");
    };
    let dal_core::UpdateKind::RequestResolved { id, answer, by } = &updates[1] else {
        panic!("grant answer must be published when resolved");
    };
    assert_eq!(opened.id, *id);
    assert_eq!(*answer, Answer::Approve);
    assert_eq!(by, &tui());
}

#[tokio::test]
async fn a_turnless_caller_rides_a_persisted_grant_but_cannot_ask() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["env"]).expect("inject");
    let caller = test_caller(inject);
    let cancel = CancellationToken::new();
    let (grant, ()) = futures::join!(store.ensure(&caller, Service::Env, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        broker.answer(id, Answer::Approve, tui()).expect("approve");
    });
    grant.expect("granted");

    let turnless = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        CallerKind::Handler,
        None,
    );
    let grant = store
        .ensure(&turnless, Service::Env, &cancel)
        .await
        .expect("a persisted grant satisfies a turnless caller");
    assert!(grant.persistent());

    let ungranted = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        ServiceSet::from_names(["net"]).expect("inject"),
        CallerKind::Handler,
        None,
    );
    let denied = store.ensure(&ungranted, Service::Net, &cancel).await;
    assert!(
        matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
        "a grant question still needs a turn: {denied:?}"
    );
    assert!(broker.open_requests().is_empty(), "no request may open");
}
