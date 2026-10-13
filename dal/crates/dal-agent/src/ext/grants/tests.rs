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
        std::num::NonZeroU32::MIN,
        CallerKind::Hook,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    )
}
fn test_mcp_caller(inject: ServiceSet) -> Caller {
    Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        std::num::NonZeroU32::MIN,
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

/// Answers and delivers the resolution, standing in for the session
/// actor's journal-then-release step that wakes the waiting gate.
fn answer_and_release(broker: &Broker, id: dal_core::RequestId, answer: Answer, by: ClientId) {
    let resolved = broker.answer(id, answer, by).expect("answer");
    broker.release(&resolved);
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
        answer_and_release(&broker, id, Answer::Approve, tui());
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
    assert_eq!(broker.open_requests(), []);
    let net_caller = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        ServiceSet::from_names(["net"]).expect("net"),
        std::num::NonZeroU32::MIN,
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
            answer_and_release(&broker, id, Answer::ApproveForSession, tui());
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
            answer_and_release(&broker, id, answer, tui());
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
    assert_eq!(store.list().await.expect("list"), []);
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
    assert_eq!(store.list().await.expect("list"), []);
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
            answer_and_release(&broker, request.id, Answer::Approve, tui());
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
    assert_eq!(broker.open_requests(), []);

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
            answer_and_release(&broker, request.id, Answer::ApproveForSession, tui());
        }
    );
    assert!(!changed.expect("changed set approved").persistent());
    assert_eq!(broker.open_requests(), []);
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
    store.set_request_update(Arc::new(move |update| {
        published
            .try_lock()
            .expect("request update callbacks do not overlap")
            .push(update);
    }));
    let answered = Arc::new(tokio::sync::Mutex::new(None));
    let answered_cell = Arc::clone(&answered);

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
            *answered_cell.lock().await = Some(request.id);
            answer_and_release(&broker, request.id, Answer::Approve, tui());
        }
    );
    assert!(first.expect("first granted").persistent());
    assert!(second.expect("second granted").persistent());

    let updates = updates.lock().await;
    // The store owns only the open broadcast: the resolution broadcast and
    // its record belong to the session actor, which this fixture omits.
    assert_eq!(updates.len(), 1);
    let dal_core::UpdateKind::RequestOpened(opened) = &updates[0] else {
        panic!("grant request must be published when opened");
    };
    let answered = answered
        .lock()
        .await
        .expect("the answered request id is recorded");
    assert_eq!(opened.id, answered);
}

fn command_caller(ext: &str, inject: ServiceSet) -> Caller {
    Caller::new(
        ext.parse::<Name>().expect("name"),
        Origin::Bundled,
        inject,
        std::num::NonZeroU32::MIN,
        CallerKind::Handler,
        None,
    )
}

#[tokio::test]
async fn a_turnless_caller_asks_once_and_rides_the_stored_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["sidecar", "turn"]).expect("inject");
    let cancel = CancellationToken::new();
    let command = command_caller("orchestration", inject);

    let (grant, ()) = futures::join!(store.ensure(&command, Service::Sidecar, &cancel), async {
        let request = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req;
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(request.turn, None, "a command question has no turn");
        assert!(
            matches!(&request.question, dal_core::Question::Grant { origin, .. }
                if &**origin == "bundled"),
            "a bundled plugin is asked like any other: {:?}",
            request.question
        );
        answer_and_release(&broker, request.id, Answer::Approve, tui());
    });
    assert!(grant.expect("the approved command is granted").persistent());
    let text = std::fs::read_to_string(dir.path().join("grants.toml")).expect("row persisted");
    assert!(text.contains("ext = \"orchestration\""), "{text}");
    assert!(text.contains("origin = \"bundled\""), "{text}");

    // The same command, and another service under the same key, ride the
    // stored row without a second question.
    for service in [Service::Sidecar, Service::Turn] {
        let grant = store
            .ensure(&command, service, &cancel)
            .await
            .expect("the stored row satisfies the command");
        assert!(grant.persistent());
    }
    assert_eq!(broker.open_requests().len(), 0, "no second question");
}

#[tokio::test]
async fn a_turnless_question_survives_turn_ends_and_declines_fail_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let inject = ServiceSet::from_names(["net"]).expect("inject");
    let cancel = CancellationToken::new();
    let command = command_caller("focus", inject);

    let (grant, ()) = futures::join!(store.ensure(&command, Service::Net, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        // A turn ending cancels that turn's requests only; this one has none.
        let ended = broker.resolve_turn(
            TurnId::new(std::num::NonZeroU64::MIN),
            Answer::Cancel,
            tui(),
        );
        assert_eq!(ended.len(), 0, "a turnless question belongs to no turn");
        answer_and_release(&broker, id, Answer::Decline, tui());
    });
    assert!(matches!(grant, Err(ServiceError::Declined)), "{grant:?}");
    assert!(
        !dir.path().join("grants.toml").exists(),
        "a decline stores nothing"
    );
}

#[tokio::test]
async fn a_question_with_no_answerer_attached_is_denied_without_a_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir, Duration::from_secs(30));
    let attached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe = Arc::clone(&attached);
    store.set_answerer(Arc::new(move || {
        probe.load(std::sync::atomic::Ordering::SeqCst)
    }));
    let inject = ServiceSet::from_names(["net"]).expect("inject");
    let cancel = CancellationToken::new();

    for caller in [command_caller("focus", inject), test_caller(inject)] {
        let denied = tokio::time::timeout(
            Duration::from_secs(5),
            store.ensure(&caller, Service::Net, &cancel),
        )
        .await
        .expect("a headless question never waits out its timeout");
        assert!(
            matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
            "{denied:?}"
        );
        assert_eq!(broker.open_requests().len(), 0, "no request for nobody");
    }

    // The denial releases the reservation: a front end that attaches
    // later is asked and its approval is stored.
    attached.store(true, std::sync::atomic::Ordering::SeqCst);
    let caller = command_caller("focus", inject);
    let (grant, ()) = futures::join!(store.ensure(&caller, Service::Net, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        answer_and_release(&broker, id, Answer::Approve, tui());
    });
    assert!(
        grant
            .expect("granted once an answerer attaches")
            .persistent()
    );
}

#[tokio::test]
async fn an_approval_before_the_data_root_exists_is_stored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = dir.path().join("fresh").join("data");
    let broker = Arc::new(Broker::new());
    let store = Arc::new(
        GrantStore::with_runtime(data.clone(), Duration::from_secs(30), broker.clone())
            .expect("a missing grants file loads empty"),
    );
    let inject = ServiceSet::from_names(["sidecar"]).expect("inject");
    let cancel = CancellationToken::new();
    let command = command_caller("orchestration", inject);
    let (grant, ()) = futures::join!(store.ensure(&command, Service::Sidecar, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        answer_and_release(&broker, id, Answer::Approve, tui());
    });
    assert!(
        grant
            .expect("the approval is stored, not lost")
            .persistent()
    );
    let path = data.join("grants.toml");
    assert!(
        path.is_file(),
        "the grants file was created with its parent"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |p: &std::path::Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data), 0o700);
    }
}
