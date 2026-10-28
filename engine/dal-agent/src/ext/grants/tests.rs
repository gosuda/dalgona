//! Grant key, broker, coalescing, and persistence tests.

use dal_core::{Name, Origin, Service};

fn key(ext: &str, origin: Origin, services: &[Service]) -> GrantKey {
    GrantKey {
        extension: ext.parse::<Name>().expect("valid name"),
        origin,
        services: ServiceSet::from_names(services.iter().map(|service| service.as_str()))
            .expect("valid set"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: std::env::temp_dir(),
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

use dal_core::{Answer, ClientId, DenyReason, ResolutionCause, ServiceSet, Timestamp, TurnId};
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
    .with_test_digest()
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
    .with_test_digest()
}

fn open_store(dir: &tempfile::TempDir) -> (Arc<GrantStore>, Arc<Broker>) {
    let broker = Arc::new(Broker::new());
    let store = Arc::new(
        GrantStore::with_runtime_scoped(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            broker.clone(),
        )
        .expect("store"),
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
    let (store, broker) = open_store(&dir);
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
    let (store, broker) = open_store(&dir);
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
        "[[grant]]\next = \"focus\"\norigin = \"user\"\nservices = [\"fs.read\", \"run\"]\ndigest = \""
    ));
    assert!(text.contains(&format!("workspace = \"{}\"", dir.path().display())));
    assert!(text.contains("by = \"tui\""));
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
        GrantStore::with_runtime_scoped(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
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
    )
    .with_test_digest();
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
    let fresh = GrantStore::with_runtime_scoped(
        fresh_dir.path().to_path_buf(),
        fresh_dir.path().to_path_buf(),
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

#[tokio::test(start_paused = true)]
async fn grant_denials_are_distinct() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
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
    let (slow, timed_out) = futures::join!(store.ensure(&caller, Service::Run, &cancel), async {
        let id = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        tokio::time::advance(Duration::from_secs(120)).await;
        id
    });
    assert!(matches!(
        slow,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));
    // The actor's sweep claims the expired question and journals it; the
    // local timer only finalizes the waiter, so nothing stays open after both.
    let expired = broker
        .expire_one(timed_out, tokio::time::Instant::now(), false)
        .expect("the sweep claims the timed-out question");
    broker.release(&expired);
    assert!(
        broker.open_requests().is_empty(),
        "no open request survives the timeout"
    );
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
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
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
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
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
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
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
    let (store, broker) = open_store(&dir);
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
            assert!(detail.as_deref().is_some_and(|text| {
                text.starts_with("search: URL https://mcp.example/search")
                    && text.contains("Code identity:")
                    && text.contains("Workspace:")
            }));
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
    let (store, broker) = open_store(&dir);
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
    .with_test_digest()
}

#[tokio::test]
async fn a_turnless_caller_asks_once_and_rides_the_stored_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
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
    let (store, broker) = open_store(&dir);
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
            ResolutionCause::Cancelled,
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
    let (store, broker) = open_store(&dir);
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
        GrantStore::with_runtime_scoped(data.clone(), data.clone(), broker.clone())
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

#[tokio::test]
async fn a_code_change_or_workspace_change_cannot_reuse_a_persistent_grant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let inject = ServiceSet::from_names(["net"]).expect("inject");
    let caller = test_caller(inject);
    let cancel = CancellationToken::new();
    let (granted, ()) = futures::join!(store.ensure(&caller, Service::Net, &cancel), async {
        let request = loop {
            if let Some(req) = broker.open_requests().into_iter().next() {
                break req.id;
            }
            tokio::task::yield_now().await;
        };
        answer_and_release(&broker, request, Answer::Approve, tui());
    });
    assert!(granted.expect("first approval").persistent());
    let reused = store
        .ensure(&caller, Service::Net, &cancel)
        .await
        .expect("the stored row satisfies the same code at the same root");
    assert!(reused.persistent(), "reuse rides the persistent row");
    assert_eq!(broker.open_requests(), [], "reuse asks nothing");

    store.set_answerer(Arc::new(|| false));
    let mut replaced_code = caller.clone();
    replaced_code.grant_digest = Some("b".repeat(64).into_boxed_str());
    assert!(matches!(
        store.ensure(&replaced_code, Service::Net, &cancel).await,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));

    let moved_workspace = GrantStore::with_runtime_scoped(
        dir.path().to_path_buf(),
        dir.path().join("different-workspace"),
        broker,
    )
    .expect("second workspace");
    moved_workspace.set_answerer(Arc::new(|| false));
    assert!(matches!(
        moved_workspace.ensure(&caller, Service::Net, &cancel).await,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));
}

fn keyed_extension(origin: Origin, inject: ServiceSet) -> crate::ext::Extension {
    crate::ext::ExtensionBuilder::new("focus", "1.0.0", inject)
        .expect("builder")
        .with_origin(origin, None)
        .with_source_digest("code-under-test".into())
        .build()
        .expect("extension")
}

#[test]
fn for_extension_keys_only_verified_user_and_bundled_code_at_an_absolute_utf8_root() {
    let inject = ServiceSet::from_names(["ask", "fs.read", "run"]).expect("inject");
    let root = std::env::temp_dir();
    for origin in [Origin::User, Origin::Bundled] {
        let key = GrantKey::for_extension(&keyed_extension(origin, inject), &root)
            .expect("a verified extension at an absolute root has a key");
        assert_eq!(key.extension.as_str(), "focus");
        assert_eq!(key.origin, origin);
        assert_eq!(
            key.services.iter().collect::<Vec<_>>(),
            vec![Service::FsRead, Service::Run],
            "ask is never part of a grant key"
        );
        assert_eq!(key.digest.len(), 64);
        assert_eq!(key.workspace, root);
    }
    assert!(
        GrantKey::for_extension(&keyed_extension(Origin::Builtin, inject), &root).is_none(),
        "builtin code needs no grant"
    );
    assert!(
        GrantKey::for_extension(
            &keyed_extension(Origin::User, inject),
            std::path::Path::new("relative/root"),
        )
        .is_none(),
        "a relative root has no grant"
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let hostile = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff-root"));
        assert!(
            GrantKey::for_extension(&keyed_extension(Origin::User, inject), &hostile).is_none(),
            "a root that is not UTF-8 has no grant"
        );
    }
}

#[tokio::test]
async fn grant_refuses_every_key_no_later_load_would_accept_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = GrantStore::new(dir.path().to_path_buf());
    let base = GrantKey {
        extension: "focus".parse().expect("name"),
        origin: Origin::User,
        services: ServiceSet::from_names(["run"]).expect("set"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
    };
    let at = Timestamp::now();
    let hostile = [
        (
            "empty service set",
            GrantKey {
                services: ServiceSet::EMPTY,
                ..base.clone()
            },
        ),
        (
            "builtin origin",
            GrantKey {
                origin: Origin::Builtin,
                ..base.clone()
            },
        ),
        (
            "digest one short",
            GrantKey {
                digest: "a".repeat(63).into_boxed_str(),
                ..base.clone()
            },
        ),
        (
            "digest one long",
            GrantKey {
                digest: "a".repeat(65).into_boxed_str(),
                ..base.clone()
            },
        ),
        (
            "digest not hex",
            GrantKey {
                digest: "g".repeat(64).into_boxed_str(),
                ..base.clone()
            },
        ),
        (
            "relative workspace",
            GrantKey {
                workspace: std::path::PathBuf::from("relative"),
                ..base.clone()
            },
        ),
    ];
    for (label, key) in hostile {
        assert!(
            matches!(
                store.grant(key, tui(), at).await,
                Err(GrantStoreError::Malformed { .. })
            ),
            "{label} must be refused as malformed"
        );
    }
    assert!(
        !dir.path().join("grants.toml").exists(),
        "a refused key writes nothing"
    );
    assert!(
        store
            .grant(base, tui(), at)
            .await
            .expect("the valid control key lands"),
        "the control proves the refusals were about the keys"
    );
}

#[tokio::test]
async fn list_orders_rows_by_extension_then_origin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = GrantStore::new(dir.path().to_path_buf());
    let at = Timestamp::now();
    let key = |extension: &str, origin| GrantKey {
        extension: extension.parse().expect("name"),
        origin,
        services: ServiceSet::from_names(["run"]).expect("set"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
    };
    for (extension, origin) in [
        ("zeta", Origin::User),
        ("alpha", Origin::User),
        ("alpha", Origin::Bundled),
    ] {
        assert!(
            store
                .grant(key(extension, origin), tui(), at)
                .await
                .expect("grant")
        );
    }
    let order: Vec<(String, Origin)> = store
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|grant| (grant.key.extension.to_string(), grant.key.origin))
        .collect();
    assert_eq!(
        order,
        [
            ("alpha".to_owned(), Origin::Bundled),
            ("alpha".to_owned(), Origin::User),
            ("zeta".to_owned(), Origin::User),
        ]
    );
}

#[tokio::test]
async fn persistent_approvals_last_ninety_days_and_a_future_stamp_never_counts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = GrantStore::new(dir.path().to_path_buf());
    let now = Timestamp::now().as_second();
    let day = 24 * 60 * 60;
    for (index, (label, offset, active)) in [
        ("approved just now", 0, true),
        ("approved a day short of expiry", -89 * day, true),
        (
            "approved 30 seconds inside the window",
            -90 * day + 30,
            true,
        ),
        ("approved 30 seconds past the window", -90 * day - 30, false),
        ("approved a year ago", -365 * day, false),
        ("stamped an hour in the future", 3_600, false),
        ("stamped a day in the future", day, false),
    ]
    .into_iter()
    .enumerate()
    {
        let key = GrantKey {
            extension: "focus".parse().expect("name"),
            origin: Origin::User,
            services: ServiceSet::from_names(["run"]).expect("set"),
            digest: format!("{index:064x}").into_boxed_str(),
            workspace: dir.path().to_path_buf(),
        };
        let at = Timestamp::from_second(now + offset).expect("in range");
        assert!(
            store.grant(key.clone(), tui(), at).await.expect("grant"),
            "{label}: a fresh key writes a row"
        );
        assert_eq!(
            store.contains(&key).await.expect("contains"),
            active,
            "{label}"
        );
    }
}

#[tokio::test]
async fn the_front_gates_answer_before_any_question_is_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let cancel = CancellationToken::new();
    let bound = Duration::from_secs(3);

    let run_only = test_caller(ServiceSet::from_names(["run"]).expect("inject"));
    let denied = tokio::time::timeout(bound, store.ensure(&run_only, Service::Net, &cancel))
        .await
        .expect("a service the extension never declared is refused at once");
    assert!(
        matches!(denied, Err(ServiceError::Denied(DenyReason::NotInjected))),
        "undeclared services are NotInjected"
    );

    let builtin = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::Builtin,
        ServiceSet::from_names(["net"]).expect("inject"),
        std::num::NonZeroU32::MIN,
        CallerKind::Hook,
        Some(TurnId::new(std::num::NonZeroU64::MIN)),
    )
    .with_test_digest();
    let free = tokio::time::timeout(bound, store.ensure(&builtin, Service::Net, &cancel))
        .await
        .expect("builtin code is never asked")
        .expect("builtin code is granted");
    assert!(!free.persistent(), "a builtin pass stores nothing");

    let asker = test_caller(ServiceSet::from_names(["ask", "run"]).expect("inject"));
    let asking = tokio::time::timeout(bound, store.ensure(&asker, Service::Ask, &cancel))
        .await
        .expect("the ask service is never itself asked")
        .expect("ask is granted");
    assert!(!asking.persistent());

    assert_eq!(broker.open_requests(), [], "no gate opened a question");
    assert!(!dir.path().join("grants.toml").exists());
}

#[tokio::test]
async fn a_caller_without_a_verified_code_identity_is_denied_without_a_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let cancel = CancellationToken::new();
    let inject = ServiceSet::from_names(["net"]).expect("inject");
    for digest in [
        None,
        Some(""),
        Some("a"),
        Some(&"a".repeat(63)[..]),
        Some(&"a".repeat(65)[..]),
        Some(&"z".repeat(64)[..]),
    ] {
        let mut caller = test_caller(inject);
        caller.grant_digest = digest.map(Box::from);
        let denied = tokio::time::timeout(
            Duration::from_secs(3),
            store.ensure(&caller, Service::Net, &cancel),
        )
        .await
        .expect("an unverified identity is refused at once");
        assert!(
            matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
            "digest {digest:?} must be NotGranted"
        );
    }
    assert_eq!(broker.open_requests(), [], "no question for a stranger");
}

#[tokio::test]
async fn legacy_and_expired_rows_do_not_authorize_calls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let grants = GrantStore::new(dir.path().to_path_buf());
    let key = GrantKey {
        extension: "focus".parse().expect("name"),
        origin: Origin::User,
        services: ServiceSet::from_names(["net"]).expect("services"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: dir.path().to_path_buf(),
    };
    grants
        .grant(key.clone(), tui(), Timestamp::UNIX_EPOCH)
        .await
        .expect("expired record");
    assert!(
        !grants
            .contains(&key)
            .await
            .expect("expired record never matches")
    );

    let legacy = "[[grant]]\next = \"focus\"\norigin = \"user\"\nservices = [\"net\"]\nby = \"tui\"\napproved_at = \"2026-01-01T00:00:00Z\"\n";
    std::fs::write(dir.path().join("grants.toml"), legacy).expect("legacy file");
    let (store, _) = open_store(&dir);
    store.set_answerer(Arc::new(|| false));
    let denied = store
        .ensure(
            &test_caller(key.services),
            Service::Net,
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(
        denied,
        Err(ServiceError::Denied(DenyReason::NotGranted))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("grants.toml")).expect("unchanged"),
        legacy
    );
}

#[tokio::test]
async fn unverified_identity_and_undeclared_service_fail_closed_without_a_request() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let cancel = CancellationToken::new();
    let inject = ServiceSet::from_names(["run"]).expect("inject");
    let bare = || {
        Caller::new(
            "focus".parse::<Name>().expect("name"),
            Origin::User,
            inject,
            std::num::NonZeroU32::MIN,
            CallerKind::Hook,
            Some(TurnId::new(std::num::NonZeroU64::MIN)),
        )
    };

    // No verified code identity: fail closed before any question opens.
    for digest in [
        None,
        Some("0".repeat(63).into_boxed_str()),
        Some("z".repeat(64).into_boxed_str()),
    ] {
        let mut caller = bare();
        caller.grant_digest = digest;
        let denied = store.ensure(&caller, Service::Run, &cancel).await;
        assert!(
            matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
            "{denied:?}"
        );
        let denied = store
            .ensure_declared_mcp(&caller, "set-a".into(), "detail".into(), &cancel)
            .await;
        assert!(
            matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
            "{denied:?}"
        );
    }

    // A service outside the inject list is denied even with a valid identity.
    let verified = bare().with_test_digest();
    let denied = store.ensure(&verified, Service::Net, &cancel).await;
    assert!(
        matches!(denied, Err(ServiceError::Denied(DenyReason::NotInjected))),
        "{denied:?}"
    );

    // A verified identity with nobody able to answer denies fast on the
    // declared-MCP path too, instead of holding for the whole ask timeout.
    store.set_answerer(Arc::new(|| false));
    let mcp = test_mcp_caller(ServiceSet::from_names(["mcp"]).expect("inject"));
    let denied = tokio::time::timeout(
        Duration::from_secs(5),
        store.ensure_declared_mcp(&mcp, "set-a".into(), "detail".into(), &cancel),
    )
    .await
    .expect("a headless MCP question never waits out its timeout");
    assert!(
        matches!(denied, Err(ServiceError::Denied(DenyReason::NotGranted))),
        "{denied:?}"
    );

    assert_eq!(
        broker.open_requests().len(),
        0,
        "no question for denied calls"
    );
    assert!(
        !dir.path().join("grants.toml").exists(),
        "denials store nothing"
    );
}

/// Waits until the broker holds exactly `count` unresolved requests and
/// returns them, oldest first.
async fn wait_open(broker: &Broker, count: usize) -> Vec<dal_core::Request> {
    loop {
        let open = broker.open_requests();
        if open.len() == count {
            return open;
        }
        tokio::task::yield_now().await;
    }
}

/// How many callers currently wait on a pending plain question.
fn waiting(store: &GrantStore) -> usize {
    store
        .lock()
        .pending
        .values()
        .map(|pending| pending.waiters)
        .sum()
}

#[tokio::test]
async fn a_dropped_opener_does_not_strand_later_callers_behind_its_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let caller = test_caller(ServiceSet::from_names(["run"]).expect("inject"));
    let cancel = CancellationToken::new();

    // A hook deadline or a cancelled scope member drops the opener while its
    // question is open, exactly like this timeout does.
    let cut = tokio::time::timeout(
        Duration::from_millis(50),
        store.ensure(&caller, Service::Run, &cancel),
    )
    .await;
    assert!(cut.is_err(), "nobody answered, so the opener was dropped");
    assert_eq!(
        broker.open_requests().len(),
        1,
        "its question is still open"
    );

    let (second, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        futures::join!(store.ensure(&caller, Service::Run, &cancel), async {
            let open = wait_open(&broker, 2).await;
            answer_and_release(&broker, open[1].id, Answer::ApproveForSession, tui());
        })
    })
    .await
    .expect("a later caller asks again instead of waiting on the abandoned question");
    assert!(!second.expect("the new question is answered").persistent());
}

#[tokio::test]
async fn a_dropped_mcp_opener_does_not_strand_later_callers_behind_its_question() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let caller = test_mcp_caller(ServiceSet::from_names(["mcp"]).expect("inject"));
    let cancel = CancellationToken::new();

    let cut = tokio::time::timeout(
        Duration::from_millis(50),
        store.ensure_declared_mcp(&caller, "set-a".into(), "detail".into(), &cancel),
    )
    .await;
    assert!(cut.is_err(), "nobody answered, so the opener was dropped");
    assert_eq!(
        broker.open_requests().len(),
        1,
        "its question is still open"
    );

    let (second, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        futures::join!(
            store.ensure_declared_mcp(&caller, "set-a".into(), "detail".into(), &cancel),
            async {
                let open = wait_open(&broker, 2).await;
                answer_and_release(&broker, open[1].id, Answer::ApproveForSession, tui());
            }
        )
    })
    .await
    .expect("a later caller asks again instead of waiting on the abandoned question");
    assert!(!second.expect("the new question is answered").persistent());
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test orchestration: the test drives its own waiter tasks"
)]
async fn a_dropped_waiter_does_not_replay_a_stale_denial_to_later_callers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let inject = ServiceSet::from_names(["run"]).expect("inject");
    let spawn_ask = |caller: Caller| {
        let store = Arc::clone(&store);
        tokio::spawn(async move {
            let cancel = CancellationToken::new();
            store.ensure(&caller, Service::Run, &cancel).await
        })
    };

    let opener = spawn_ask(test_caller(inject));
    let first = wait_open(&broker, 1).await;
    let waiter = spawn_ask(test_caller(inject));
    tokio::time::timeout(Duration::from_secs(5), async {
        while waiting(&store) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second caller joined the open question");

    // The waiter's future is dropped while its registration is live.
    waiter.abort();
    let cut = waiter.await.expect_err("the waiter was cut off");
    assert!(cut.is_cancelled());
    assert_eq!(
        waiting(&store),
        0,
        "the dropped waiter left no registration"
    );

    answer_and_release(&broker, first[0].id, Answer::Decline, tui());
    let declined = opener.await.expect("the opener task finishes");
    assert!(
        matches!(declined, Err(ServiceError::Declined)),
        "{declined:?}"
    );

    // A decline is one terminal outcome, not a stored verdict: the next call
    // asks again.
    let cancel = CancellationToken::new();
    let retry = test_caller(inject);
    let (third, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        futures::join!(store.ensure(&retry, Service::Run, &cancel), async {
            let open = wait_open(&broker, 1).await;
            answer_and_release(&broker, open[0].id, Answer::ApproveForSession, tui());
        })
    })
    .await
    .expect("the later caller is asked instead of replaying the decline");
    assert!(third.is_ok(), "{third:?}");
}

#[tokio::test]
async fn an_approval_cannot_restore_a_grant_revoked_while_it_was_pending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let admin = GrantStore::new(dir.path().to_path_buf());
    let other = key("other", Origin::User, &[Service::FsRead]);
    admin
        .grant(other.clone(), tui(), Timestamp::now())
        .await
        .expect("a stored approval for another extension");
    let caller = test_caller(ServiceSet::from_names(["run"]).expect("inject"));
    let cancel = CancellationToken::new();

    let (granted, ()) = futures::join!(store.ensure(&caller, Service::Run, &cancel), async {
        // The question opened after the store read the file, which still held
        // the other extension's row; an administrator revokes it now.
        let open = wait_open(&broker, 1).await;
        let name: Name = "other".parse().expect("name");
        assert_eq!(admin.revoke(&name).await.expect("revoke"), 1);
        answer_and_release(&broker, open[0].id, Answer::Approve, tui());
    });
    assert!(granted.expect("approved").persistent());

    assert!(
        !admin.contains(&other).await.expect("contains"),
        "the approval wrote back a stale copy of the file"
    );
    assert_eq!(
        admin.list().await.expect("the file still loads").len(),
        1,
        "only the new approval is stored"
    );
}

#[tokio::test]
async fn an_mcp_approval_for_an_extension_with_no_capabilities_keeps_the_file_loadable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let caller = test_mcp_caller(ServiceSet::EMPTY);
    let cancel = CancellationToken::new();

    let (approved, ()) = futures::join!(
        store.ensure_declared_mcp(&caller, "set-a".into(), "detail".into(), &cancel),
        async {
            let open = wait_open(&broker, 1).await;
            answer_and_release(&broker, open[0].id, Answer::Approve, tui());
        }
    );
    assert!(approved.expect("approved").persistent());

    // Every later check reloads the file; an unloadable row would fail all of
    // them for every extension.
    let listed = store
        .list()
        .await
        .expect("the file the approval wrote still loads");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].mcp_set.as_deref(), Some("set-a"));
    assert!(listed[0].key.services.is_empty());
    assert!(
        store
            .ensure_declared_mcp(&caller, "set-a".into(), "detail".into(), &cancel)
            .await
            .expect("the stored row answers the same declaration")
            .persistent()
    );
    assert_eq!(broker.open_requests(), []);

    // Only a declared-MCP row may carry no service; any other row that
    // authorizes nothing still fails closed.
    let bare = "[[grant]]\next = \"focus\"\norigin = \"user\"\nservices = []\nby = \"tui\"\napproved_at = \"2026-01-01T00:00:00Z\"\n";
    assert!(matches!(
        super::persist::parse_rows(bare),
        Err(GrantStoreError::Malformed { .. })
    ));
}

#[tokio::test]
async fn revoke_invalidates_a_late_mcp_answer_like_a_plain_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let inject = ServiceSet::from_names(["mcp"]).expect("inject");
    let caller_a = test_mcp_caller(inject);
    let caller_b = test_mcp_caller(inject);
    let cancel = CancellationToken::new();
    let ext: Name = "focus".parse().expect("name");

    let (first, second, ()) = futures::join!(
        store.ensure_declared_mcp(&caller_a, "set-a".into(), "detail".into(), &cancel),
        store.ensure_declared_mcp(&caller_b, "set-a".into(), "detail".into(), &cancel),
        async {
            let open = wait_open(&broker, 1).await;
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            store.revoke(&ext).await.expect("revoke");
            answer_and_release(&broker, open[0].id, Answer::Approve, tui());
        }
    );
    assert!(matches!(first, Err(ServiceError::Cancelled)), "{first:?}");
    assert!(matches!(second, Err(ServiceError::Cancelled)), "{second:?}");
    assert!(
        !dir.path().join("grants.toml").exists(),
        "the late approval stored nothing"
    );
}

#[tokio::test]
async fn grant_rejects_a_key_that_cannot_authorize_and_names_the_fix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = GrantStore::new(dir.path().to_path_buf());
    let good = key("focus", Origin::User, &[Service::Run]);
    let cases = [
        (
            GrantKey {
                services: ServiceSet::EMPTY,
                ..good.clone()
            },
            "no service",
        ),
        (
            GrantKey {
                origin: Origin::Builtin,
                ..good.clone()
            },
            "builtin",
        ),
        (
            GrantKey {
                digest: "z".repeat(64).into_boxed_str(),
                ..good.clone()
            },
            "digest",
        ),
        (
            GrantKey {
                digest: "a".repeat(63).into_boxed_str(),
                ..good.clone()
            },
            "digest",
        ),
        (
            GrantKey {
                workspace: std::path::PathBuf::from("relative/root"),
                ..good.clone()
            },
            "workspace",
        ),
    ];
    for (bad, needle) in cases {
        let error = store
            .grant(bad, tui(), Timestamp::now())
            .await
            .expect_err("the key is rejected");
        assert!(
            matches!(error, GrantStoreError::Malformed { .. }),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains(needle), "{text}");
        assert!(
            text.contains("grant key rejected"),
            "the text says the key, not the file, was refused: {text}"
        );
    }
    assert!(
        !dir.path().join("grants.toml").exists(),
        "a rejected key stores nothing"
    );
}

#[tokio::test]
async fn nested_eval_approval() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let inject = ServiceSet::from_names(["run"]).expect("inject");
    let cancel = CancellationToken::new();
    let turn = Some(TurnId::new(std::num::NonZeroU64::MIN));
    let cell = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        std::num::NonZeroU32::MIN,
        CallerKind::Cell { approved: true },
        turn,
    )
    .with_test_digest();
    // The approved cell eval starts with its own approval as its grant: no
    // broker question opens for the cell itself.
    let granted = store
        .ensure(&cell, Service::Run, &cancel)
        .await
        .expect("an approved cell needs no second question");
    assert!(!granted.persistent(), "nothing is stored for a cell");
    assert_eq!(
        broker.open_requests().len(),
        0,
        "the cell approval opens nothing"
    );
    // A nested tool call made while that cell runs is a different caller:
    // it must open exactly one broker approval, and a decline there must
    // return into the cell as a denial rather than stalling the cell.
    let nested = Caller::new(
        "focus".parse::<Name>().expect("name"),
        Origin::User,
        inject,
        std::num::NonZeroU32::MIN,
        CallerKind::Tool,
        turn,
    )
    .with_test_digest();
    let (outcome, ()) = futures::join!(store.ensure(&nested, Service::Run, &cancel), async {
        let open = wait_open(&broker, 1).await;
        assert_eq!(open.len(), 1, "the nested ask opens inside the cell eval");
        answer_and_release(&broker, open[0].id, Answer::Decline, tui());
    });
    assert!(
        matches!(outcome, Err(ServiceError::Declined)),
        "the declined nested approval denies into the running cell: {outcome:?}"
    );
    assert_eq!(
        broker.open_requests().len(),
        0,
        "the nested denial settles its question"
    );
    // The cell itself still rides its own approval after the nested denial.
    let again = store
        .ensure(&cell, Service::Run, &cancel)
        .await
        .expect("the cell keeps its own grant after a nested denial");
    assert!(!again.persistent());
    assert_eq!(broker.open_requests().len(), 0);
}

#[tokio::test]
async fn only_an_approved_cell_rides_its_own_approval() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, broker) = open_store(&dir);
    let inject = ServiceSet::from_names(["run"]).expect("inject");
    let cancel = CancellationToken::new();
    let cell = |approved: bool| {
        Caller::new(
            "focus".parse::<Name>().expect("name"),
            Origin::User,
            inject,
            std::num::NonZeroU32::MIN,
            CallerKind::Cell { approved },
            Some(TurnId::new(std::num::NonZeroU64::MIN)),
        )
        .with_test_digest()
    };

    // The ladder approved this cell: its approval is its grant.
    let granted = store
        .ensure(&cell(true), Service::Run, &cancel)
        .await
        .expect("an approved cell needs no second question");
    assert!(!granted.persistent(), "nothing is stored for a cell");
    assert_eq!(broker.open_requests().len(), 0);

    // A cell nobody approved has no grant of its own: it is asked like any
    // other caller.
    let false_cell = cell(false);
    let (outcome, ()) = futures::join!(store.ensure(&false_cell, Service::Run, &cancel), async {
        let open = wait_open(&broker, 1).await;
        answer_and_release(&broker, open[0].id, Answer::Decline, tui());
    });
    assert!(
        matches!(outcome, Err(ServiceError::Declined)),
        "{outcome:?}"
    );
}
